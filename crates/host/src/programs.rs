//! Programs BeatBox knows how to add to a Linux guest, from a list of
//! recipes.
//!
//! A recipe gets its program one of two ways: from the guest's own system
//! (apt), or as one file from the program's releases, pinned by its sha256.
//! A guest that can't reach the internet still gets it: this computer
//! downloads, checks each file against the fingerprint it was given, and the
//! agent copies the files in. The guest stays offline. For apt, the guest
//! itself says which files it needs, from its own signed package lists.
//!
//! Downloading happens on a thread; everything that needs the guest happens
//! when the page asks how it is going, one step per ask, so no action holds
//! the agent for long.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beatbox_engine::{Action, Data, ProviderError, data};
use beatbox_reasons::{Reason, reason};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::HostSandbox;

/// The recipes BeatBox brings.
const BUILT_IN: &str = include_str!("../programs.json");

/// Where files wait inside the guest on their way to being installed, and
/// where a program BeatBox added leaves its mark (its version).
const INCOMING: &str = "/var/lib/beatbox/programs/incoming";

/// Room asked for beyond what is downloaded. A program takes more room
/// installed than packed, and apt wants space to work.
// ponytail: four times the download plus 50 MB, measured on nothing better
// than mariadb (20 MB in, 180 MB installed); apt's Installed-Size if it misleads.
fn room_needed_kb(bytes: u64) -> u64 {
    bytes * 4 / 1024 + 50 * 1024
}

/// What the guest has, and what each recipe would take, as `key=value`
/// lines. Every argument is `id|command|package`, `-` without a package.
/// Read only: it never runs `apt-get update`.
const LIST: &str = r#"export LC_ALL=C
os=/etc/os-release
printf 'system=%s\n' "$(sed -n 's/^PRETTY_NAME=//p' "$os" 2>/dev/null | tr -d '"')"
printf 'arch=%s\n' "$(uname -m)"
printf 'free_kb=%s\n' "$(df -Pk / | awk 'NR==2 {print $4}')"
d=/var/log/beatbox
[ "$(cat "$d/programs.state" 2>/dev/null)" = running ] \
  && kill -0 "$(cat "$d/programs.pid" 2>/dev/null)" 2>/dev/null && echo busy=yes
apt=
if command -v apt-get >/dev/null 2>&1; then
  apt=yes
  echo apt=yes
  source=$(cat /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources 2>/dev/null \
    | grep -v '^[[:space:]]*#' | sed -n 's#.*\(https\{0,1\}\)://\([A-Za-z0-9.-]*\).*#\1 \2#p' | head -n 1)
  port=80
  case "$source" in https*) port=443 ;; esac
  if [ -n "$source" ] && timeout 4 bash -c 'exec 3<>"/dev/tcp/$1/$2"' bash "${source#* }" "$port" 2>/dev/null; then
    echo reach=yes
  fi
  ls /var/lib/apt/lists 2>/dev/null | grep -q '_Packages' && echo lists=yes
fi
for entry; do
  id=${entry%%|*}
  rest=${entry#*|}
  cmd=${rest%%|*}
  pkg=${rest#*|}
  mine=no
  [ -e "/var/lib/beatbox/programs/$id" ] && mine=yes
  if [ "$pkg" != - ] && [ -n "$apt" ]; then
    v=$(dpkg-query -W -f='${Status} ${Version}' "$pkg" 2>/dev/null)
    case "$v" in "install ok installed "*) echo "on=$id $mine ${v##* }"; continue ;; esac
  fi
  if command -v "$cmd" >/dev/null 2>&1; then
    echo "on=$id $mine $(cat "/var/lib/beatbox/programs/$id" 2>/dev/null)"
    continue
  fi
  { [ "$pkg" = - ] || [ -z "$apt" ]; } && continue
  c=$(apt-cache policy "$pkg" 2>/dev/null | sed -n 's/^ *Candidate: //p')
  if [ -z "$c" ] || [ "$c" = "(none)" ]; then
    echo "absent=$id"
    continue
  fi
  u=$(apt-get install --print-uris -qq --no-install-recommends "$pkg" 2>/dev/null)
  echo "can=$id $c $(printf '%s\n' "$u" | grep -c "^'") $(printf '%s\n' "$u" | awk '{s+=$3} END {print s+0}')"
done"#;

/// The files package `$1` needs, one `file=url bytes sha256 name` line each.
/// The sha256 comes from the guest's own lists, which apt checked against
/// the distribution's signature when it fetched them.
const WANTS: &str = r#"export LC_ALL=C
apt-get install --print-uris -qq --no-install-recommends "$1" 2>/dev/null | while read -r url name bytes rest; do
  url=${url#\'}
  url=${url%\'}
  sha=$(apt-cache show --no-all-versions "${name%%_*}" 2>/dev/null | sed -n 's/^SHA256: //p' | head -n 1)
  echo "file=$url $bytes $sha $name"
done"#;

/// The package indexes the guest's lists hold, one `base file metakey
/// by-hash` line each. Read only.
const INDEXES: &str = r#"export LC_ALL=C
apt-get indextargets --format '$(BASE_URI) $(FILENAME) $(METAKEY) $(BY_HASH)' 'Created-By: Packages' 2>/dev/null"#;

/// Check the `InRelease` files copied in, and say which indexes changed.
/// Every argument is `release|metakey|target`. One `want=release metakey
/// sha256 bytes target` line per `.xz` index to fetch; an index whose
/// current file already matches is left alone. 4: a signature that doesn't
/// verify; 5: a release older than the one the guest has.
// Safe: only the signed text is read (gpgv --output), with the keys apt
// already trusts for the whole system; the removed-keys rings never match
// the glob. A replayed old release is refused by its Date.
const LISTS_CHECK: &str = r#"export LC_ALL=C
in=/var/lib/beatbox/programs/incoming
lists=/var/lib/apt/lists
t=$(mktemp -d) || exit 1
trap 'rm -rf "$t"' EXIT
for entry; do
  name=${entry%%|*}
  rest=${entry#*|}
  meta=${rest%%|*}
  target=${rest#*|}
  plain=$t/$name
  if [ ! -e "$plain" ]; then
    for k in /usr/share/keyrings/*-archive-keyring.gpg /etc/apt/trusted.gpg.d/*.gpg; do
      [ -f "$k" ] || continue
      rm -f "$plain"
      gpgv --keyring "$k" --output "$plain" "$in/$name" 2>/dev/null && break
      rm -f "$plain"
    done
    [ -s "$plain" ] || exit 4
    old=$(sed -n 's/^Date: //p' "$lists/$name" 2>/dev/null | head -n 1)
    new=$(sed -n 's/^Date: //p' "$plain" | head -n 1)
    if [ -n "$old" ] && [ "$(date -d "$new" +%s 2>/dev/null || echo 0)" -lt "$(date -d "$old" +%s 2>/dev/null || echo 0)" ]; then
      exit 5
    fi
  fi
  hash_of() { awk -v k="$1" '/^SHA256:/ {s=1; next} /^[^ ]/ {s=0} s && $3 == k {print $1, $2; exit}' "$plain"; }
  packed=$(hash_of "$meta.xz")
  [ -n "$packed" ] || continue
  whole=$(hash_of "$meta")
  [ -e "$target" ] && [ "$(sha256sum <"$target" | cut -d ' ' -f 1)" = "${whole% *}" ] && continue
  echo "want=$name $meta $packed $target"
done"#;

/// Put the refreshed lists in place. Arguments: `R|release` for each
/// `InRelease`, `I|sha256|target` for each index. Every index is unpacked
/// before any file moves, so a failure leaves the old lists whole.
// ponytail: no apt lock taken; an apt-get running at the same moment could
// read a half-swapped set. Take /var/lib/apt/lists/lock if that ever bites.
const LISTS_APPLY: &str = r#"export LC_ALL=C
in=/var/lib/beatbox/programs/incoming
lists=/var/lib/apt/lists
for entry; do
  case "$entry" in
    I\|*)
      e=${entry#I|}
      xz -dc "$in/${e%%|*}" >"${e#*|}.beatbox" || { rm -f "$lists"/*.beatbox; exit 1; } ;;
  esac
done
for entry; do
  case "$entry" in
    I\|*) e=${entry#I|}; mv -f "${e#*|}.beatbox" "${e#*|}" ;;
    R\|*) install -m 0644 "$in/${entry#R|}" "$lists/${entry#R|}" ;;
  esac
done
rm -rf "$in""#;

/// Start [`INSTALL`] (`$1`) with the rest of the arguments, detached from
/// the agent: apt can take minutes, and the agent answers one thing at a
/// time. 3 when one is already under way.
const APPLY: &str = r#"d=/var/log/beatbox
mkdir -p "$d" || exit 1
if [ "$(cat "$d/programs.state" 2>/dev/null)" = running ] \
  && kill -0 "$(cat "$d/programs.pid" 2>/dev/null)" 2>/dev/null; then exit 3; fi
work=$1
shift
echo running >"$d/programs.state"
setsid /bin/sh -c "$work" sh "$@" >"$d/programs.log" 2>&1 </dev/null &
echo $! >"$d/programs.pid""#;

/// The install: `$1` how (`apt`, `debs` or `file`), `$2` the recipe, `$3`
/// the version to remember, `$4` units to keep off, then what to install.
/// `apt` takes a package and needs the internet; `debs` takes the package
/// and the files already copied in; `file` takes an archive, the program inside it (`-`: the file
/// is the program) and the command to install it as. No service of theirs
/// starts while it runs, and theirs stay off after: a Server runs its own.
/// The mark it leaves holds the version, then every package that came in,
/// so removing it takes those along and nothing else.
const INSTALL: &str = r#"export DEBIAN_FRONTEND=noninteractive LC_ALL=C
set -f
d=/var/log/beatbox
p=/var/lib/beatbox/programs
how=$1 id=$2 version=$3 units=$4
shift 4
installed() {
  command -v dpkg-query >/dev/null 2>&1 || return 0
  dpkg-query -W -f='${db:Status-Abbrev} ${Package}\n' 2>/dev/null | awk '$1 == "ii" {print $2}' | sort
}
before=$(mktemp) && installed >"$before"
mine=
if [ "$how" != file ] && [ ! -e /usr/sbin/policy-rc.d ]; then
  printf '#!/bin/sh\nexit 101\n' >/usr/sbin/policy-rc.d && chmod 755 /usr/sbin/policy-rc.d && mine=yes
fi
wait=DPkg::Lock::Timeout=120
keep=Dpkg::Options::=--force-confold
case "$how" in
  apt) apt-get -o "$wait" update && apt-get -o "$wait" install -y --no-install-recommends -o "$keep" "$1" ;;
  debs)
    # In apt's own cache under the names apt gave them, apt takes them as
    # downloaded and still checks each against its lists. Passed as paths,
    # it would want the same version from the mirror instead.
    pkg=$1
    shift
    cache=/var/cache/apt/archives
    cp -- "$@" "$cache/" && apt-get -o "$wait" install -y --no-download --no-install-recommends -o "$keep" "$pkg"
    code=$?
    for f; do rm -f "$cache/${f##*/}"; done
    [ "$code" -eq 0 ] ;;
  file)
    if [ "$2" = - ]; then
      install -m 0755 "$1" "/usr/local/bin/$3"
    else
      t=$(mktemp -d) && tar -xzf "$1" -C "$t" "$2" && install -m 0755 "$t/$2" "/usr/local/bin/$3"
      code=$?
      rm -rf "$t"
      [ "$code" -eq 0 ]
    fi ;;
  *) false ;;
esac
code=$?
[ -n "$mine" ] && rm -f /usr/sbin/policy-rc.d
rm -rf "$p/incoming"
if [ "$code" -ne 0 ]; then
  rm -f "$before"
  echo failed >"$d/programs.state"
  exit "$code"
fi
for unit in $units; do
  systemctl disable --now "$unit" >/dev/null 2>&1
done
{ printf '%s\n' "$version"; installed | comm -13 "$before" -; } >"$p/$id"
rm -f "$before"
echo done >"$d/programs.state""#;

/// How the install is going: `state` (none, running, done or failed;
/// running with its process gone is failed) and the end of its log.
const STATE: &str = r#"d=/var/log/beatbox
state=$(cat "$d/programs.state" 2>/dev/null) || { echo state=none; exit 0; }
if [ "$state" = running ] && ! kill -0 "$(cat "$d/programs.pid" 2>/dev/null)" 2>/dev/null; then
  state=failed
fi
echo "state=$state"
tail -n 20 "$d/programs.log" 2>/dev/null | sed 's/^/line=/'
exit 0"#;

/// Remove package `$1`, then whatever it brought (listed in mark `$2`
/// after the version) that nothing else needs now. Orphans that were
/// there before stay.
const REMOVE: &str = r#"export DEBIAN_FRONTEND=noninteractive LC_ALL=C
set -f
wait=DPkg::Lock::Timeout=60
apt-get -o "$wait" remove -y "$1" || exit 1
came=$(tail -n +2 "$2" 2>/dev/null)
gone=
for pkg in $(apt-get -s autoremove 2>/dev/null | sed -n 's/^Remv \([^ ]*\).*/\1/p'); do
  printf '%s\n' "$came" | grep -qx -- "$pkg" && gone="$gone $pkg"
done
if [ -n "$gone" ]; then
  apt-get -o "$wait" remove -y $gone || exit 1
fi
rm -f "$2""#;

/// Run `"$@"` and keep the first line it says, its exit code kept.
const TRY: &str = r#"out=$("$@" 2>&1)
code=$?
printf '%s\n' "$out" | head -n 1
exit "$code""#;

/// One recipe.
#[derive(Clone, Debug)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub group: String,
    /// What it is, for a recipe somebody wrote: the built-ins' are words of
    /// the page's own.
    pub what: String,
    pub command: String,
    pub try_it: Vec<String>,
    pub apt: Option<String>,
    pub keep_off: Vec<String>,
    pub file: Option<Release>,
    pub own: bool,
}

/// A program that is one file from its own releases.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The program's path inside the archive; `None` when the file is the
    /// program itself.
    pub inside: Option<String>,
    /// For each kind of machine (`uname -m`): where, its sha256, its size.
    pub builds: Map<String, Value>,
}

fn words(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn simple(text: &str, extra: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

impl Recipe {
    /// A recipe from its JSON, refused when anything in it would be run or
    /// fetched without being exactly what it should: the guest runs these
    /// as root.
    pub fn parse(value: &Value, own: bool) -> Result<Self, Reason> {
        let bad = |what: &str| reason!("host.bad_program", field = what);
        let id = field(value, "id");
        if !simple(&id, "-") || id.starts_with('-') {
            return Err(bad("id"));
        }
        let name = field(value, "name");
        if name.is_empty() || name.chars().count() > 40 {
            return Err(bad("name"));
        }
        let command = field(value, "command");
        if !simple(&command, "._+-") || command.starts_with('-') {
            return Err(bad("command"));
        }
        let try_it = words(value.get("try"));
        if try_it.is_empty() {
            return Err(bad("try"));
        }
        let apt = value.get("apt").and_then(Value::as_str).map(str::to_owned);
        if apt
            .as_deref()
            .is_some_and(|package| !simple(package, ".+-") || package.starts_with('-'))
        {
            return Err(bad("apt"));
        }
        let keep_off = words(value.get("keep_off"));
        if keep_off.iter().any(|unit| !simple(unit, ".@_-")) {
            return Err(bad("keep_off"));
        }
        let file = match value.get("file") {
            Some(release) => {
                let builds: Map<String, Value> = ["x86_64", "aarch64"]
                    .iter()
                    .filter_map(|arch| release.get(*arch).map(|b| ((*arch).to_owned(), b.clone())))
                    .collect();
                for build in builds.values() {
                    let url = field(build, "url");
                    let sha = field(build, "sha256");
                    if !url.starts_with("https://") || url.chars().any(char::is_whitespace) {
                        return Err(bad("url"));
                    }
                    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Err(bad("sha256"));
                    }
                }
                if builds.is_empty() {
                    return Err(bad("url"));
                }
                let inside = Some(field(release, "inside")).filter(|inside| !inside.is_empty());
                if inside
                    .as_deref()
                    .is_some_and(|inside| !simple(inside, "./_+-") || inside.contains(".."))
                {
                    return Err(bad("inside"));
                }
                Some(Release {
                    version: field(release, "version"),
                    inside,
                    builds,
                })
            }
            None => None,
        };
        if apt.is_some() == file.is_some() {
            return Err(bad("apt"));
        }
        Ok(Self {
            id,
            name,
            group: if own {
                "yours".into()
            } else {
                field(value, "group")
            },
            what: field(value, "what"),
            command,
            try_it,
            apt,
            keep_off,
            file,
            own,
        })
    }

    /// What a page is told about it before the guest is asked.
    fn row(&self) -> Value {
        let source = self
            .file
            .as_ref()
            .and_then(|release| release.builds.values().next())
            .map(|build| field(build, "url"))
            .and_then(|url| url.split('/').nth(2).map(str::to_owned))
            .unwrap_or_default();
        json!({
            "id": self.id, "name": self.name, "group": self.group, "what": self.what,
            "own": self.own, "command": self.command,
            "how": if self.apt.is_some() { "apt" } else { "file" },
            "package": self.apt, "source": source, "kept_off": !self.keep_off.is_empty(),
            "path": if self.file.is_some() { format!("/usr/local/bin/{}", self.command) } else { String::new() },
            "on": false, "mine": false, "version": "", "files": 0, "bytes": 0,
            // Why it can't be added here, when it can't.
            "cannot": Value::Null,
        })
    }

    fn json(&self) -> Value {
        let mut value = json!({
            "id": self.id, "name": self.name, "what": self.what,
            "command": self.command, "try": self.try_it,
        });
        if let Some(package) = &self.apt {
            value["apt"] = json!(package);
        }
        if let Some(release) = &self.file {
            let mut file = release.builds.clone();
            file.insert("version".into(), json!(release.version));
            if let Some(inside) = &release.inside {
                file.insert("inside".into(), json!(inside));
            }
            value["file"] = Value::Object(file);
        }
        value
    }
}

/// The built-in recipes.
pub fn built_in() -> Vec<Recipe> {
    let list: Value = serde_json::from_str(BUILT_IN).expect("programs.json is JSON");
    list.as_array()
        .expect("programs.json is a list")
        .iter()
        .map(|value| Recipe::parse(value, false).expect("built-in recipes are valid"))
        .collect()
}

/// `1:2.43.0-1ubuntu7.3` -> `2.43.0`: the upstream version, as a person
/// would say it. Debian's epoch and revision mean nothing to them.
pub fn plain_version(version: &str) -> String {
    let version = version.split_once(':').map_or(version, |(_, rest)| rest);
    version
        .split(['-', '+', '~'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// A file to fetch.
#[derive(Clone, Debug)]
struct Wanted {
    url: String,
    name: String,
    bytes: u64,
    sha256: String,
}

/// How the download thread left it.
#[derive(Debug, Default)]
struct Fetch {
    fetched: usize,
    done: bool,
    failed: Option<Reason>,
    stop: bool,
}

/// One package index of the guest's lists.
#[derive(Clone, Debug)]
struct Index {
    /// The `InRelease` it belongs to, as named in the lists folder.
    release: String,
    base: String,
    meta: String,
    target: String,
    by_hash: bool,
    /// The `.xz` to fetch, once the release says it changed.
    sha256: Option<String>,
}

/// One program being added.
#[derive(Debug)]
pub struct Job {
    recipe: Recipe,
    /// `lists` and `indexes` while the guest's package lists are refreshed
    /// first, then `debs`; or `apt` or `file`.
    how: &'static str,
    indexes: Vec<Index>,
    /// How much the refreshed lists took to bring in.
    list_bytes: u64,
    free_kb: u64,
    version: String,
    files: Vec<Wanted>,
    folder: PathBuf,
    fetch: Arc<Mutex<Fetch>>,
    copied: usize,
    phase: &'static str,
    failed: Option<Reason>,
    said: String,
    lines: Vec<String>,
}

/// Fetch every file into `folder`, checking each, until done or told to
/// stop. `curl` is this computer's own (Windows has it since 2018): no HTTP
/// stack of ours to keep secure.
fn fetch(files: &[Wanted], folder: &Path, state: &Mutex<Fetch>) -> Result<(), Reason> {
    std::fs::create_dir_all(folder).map_err(|e| reason!("host.download_failed", detail = e))?;
    for wanted in files {
        let part = folder.join(format!("{}.part", wanted.name));
        let mut child = Command::new("curl")
            .args(["-fsSL", "--retry", "2", "--connect-timeout", "15", "-o"])
            .arg(&part)
            .arg(&wanted.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => reason!("host.no_curl"),
                _ => reason!("host.download_failed", detail = error),
            })?;
        let status = loop {
            if state.lock().expect("fetch state").stop {
                let _ = child.kill();
                let _ = child.wait();
                return Err(reason!("host.program_stopped"));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(error) => return Err(reason!("host.download_failed", detail = error)),
            }
        };
        match status.code() {
            Some(0) => {}
            // Could not resolve, connect, finish in time, or speak TLS.
            Some(6 | 7 | 28 | 35) => return Err(reason!("host.no_internet_here")),
            Some(22) => return Err(reason!("host.files_gone")),
            code => {
                return Err(reason!(
                    "host.download_failed",
                    detail = format!("curl {}", code.unwrap_or(-1))
                ));
            }
        }
        let mut file =
            std::fs::File::open(&part).map_err(|e| reason!("host.download_failed", detail = e))?;
        // An InRelease has no fingerprint to come with: the guest checks its
        // signature instead.
        if wanted.sha256.is_empty() {
            std::fs::rename(&part, folder.join(&wanted.name))
                .map_err(|e| reason!("host.download_failed", detail = e))?;
            state.lock().expect("fetch state").fetched += 1;
            continue;
        }
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|e| reason!("host.download_failed", detail = e))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let got: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !got.eq_ignore_ascii_case(&wanted.sha256) {
            let _ = std::fs::remove_file(&part);
            return Err(reason!("host.wrong_file", file = wanted.name));
        }
        std::fs::rename(&part, folder.join(&wanted.name))
            .map_err(|e| reason!("host.download_failed", detail = e))?;
        state.lock().expect("fetch state").fetched += 1;
    }
    Ok(())
}

/// Read [`INDEXES`]. What comes back becomes paths and URLs here and
/// arguments there, so anything unusual is dropped.
fn indexes_of(text: &str) -> Vec<Index> {
    let plain = |text: &str, extra: &str| {
        !text.is_empty()
            && text.len() <= 255
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            let [base, file, meta, by_hash] = parts[..] else {
                return None;
            };
            let name = file.strip_prefix("/var/lib/apt/lists/")?;
            let stem = name.strip_suffix(&format!("_{}", meta.replace('/', "_")))?;
            let web = base.starts_with("http://") || base.starts_with("https://");
            (web && base.ends_with('/')
                && plain(base, ":/._-~%+")
                && plain(name, "._-~%+")
                && plain(meta, "/._-"))
            .then(|| Index {
                release: format!("{stem}_InRelease"),
                base: base.to_owned(),
                meta: meta.to_owned(),
                target: file.to_owned(),
                by_hash: by_hash == "yes",
                sha256: None,
            })
        })
        .collect()
}

/// Fetch the job's files on a thread of their own, from a clean folder.
fn start_fetch(job: &mut Job) -> Result<(), ProviderError> {
    let _ = std::fs::remove_dir_all(&job.folder);
    job.fetch = Arc::default();
    job.copied = 0;
    job.phase = "download";
    let (files, folder, state) = (job.files.clone(), job.folder.clone(), job.fetch.clone());
    std::thread::Builder::new()
        .name(format!("beatbox-fetch-{}", job.recipe.id))
        .spawn(move || {
            let outcome = fetch(&files, &folder, &state);
            let mut state = state.lock().expect("fetch state");
            state.done = true;
            state.failed = outcome.err();
        })
        .map_err(|e| ProviderError::new(reason!("host.download_failed", detail = e)))?;
    Ok(())
}

fn lines_of(said: &Data) -> Vec<(String, String)> {
    said.get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn code_of(said: &Data) -> i64 {
    said.get("exit_code").and_then(Value::as_i64).unwrap_or(-1)
}

fn stdout_of(said: &Data) -> String {
    said.get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

impl HostSandbox {
    /// Where recipes somebody wrote are kept, one file each: the setting,
    /// or `programs/` beside `machines/` for a machine made before it.
    fn own_programs(&self) -> Option<PathBuf> {
        if let Some(folder) = self
            .config
            .setting("programs")
            .and_then(Value::as_str)
            .filter(|folder| !folder.is_empty())
        {
            return Some(PathBuf::from(folder));
        }
        let machines = self.workspace.parent()?;
        (machines.file_name()? == "machines")
            .then(|| machines.parent().map(|home| home.join("programs")))?
    }

    /// Every recipe: the built-ins, then the ones somebody wrote. One with a
    /// built-in's id is left out.
    fn recipes(&self) -> Vec<Recipe> {
        let mut all = built_in();
        let mut files: Vec<PathBuf> = self
            .own_programs()
            .and_then(|folder| std::fs::read_dir(folder).ok())
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            match serde_json::from_str(&text).map_err(|e| reason!("host.bad_program", field = e)) {
                Ok(value) => match Recipe::parse(&value, true) {
                    Ok(recipe) if all.iter().all(|r| r.id != recipe.id) => all.push(recipe),
                    Ok(_) => {}
                    Err(why) => log::warn!("{} left out: {why:?}", path.display()),
                },
                Err(why) => log::warn!("{} left out: {why:?}", path.display()),
            }
        }
        all
    }

    fn recipe(&self, action: &Action) -> Result<Recipe, ProviderError> {
        let id = Self::required(action, "program")?;
        self.recipes()
            .into_iter()
            .find(|recipe| recipe.id == id)
            .ok_or_else(|| ProviderError::new(reason!("host.no_program", program = id)))
    }

    /// Run `script` with `parameters` through the guest's `sh`.
    fn guest_sh(
        &mut self,
        capability: &str,
        script: &str,
        parameters: &[&str],
        seconds: u64,
    ) -> Result<Data, ProviderError> {
        let mut arguments = vec![json!("-c"), json!(script), json!("sh")];
        arguments.extend(parameters.iter().map(|p| json!(p)));
        self.with_agent(capability, |agent| {
            agent.run("/bin/sh", &arguments, Some(Duration::from_secs(seconds)))
        })
    }

    /// What the guest has and what each recipe would take. Looked at, never
    /// changed.
    /// `quick`: only the recipes, without asking the guest.
    pub(super) fn programs_list(&mut self, quick: bool) -> Result<Data, ProviderError> {
        let recipes = self.recipes();
        if quick {
            let rows: Vec<Value> = recipes.iter().map(Recipe::row).collect();
            return Ok(data!({"windows": false, "programs": rows}));
        }
        if self.guest_family("programs.list")? == "windows" {
            return Ok(data!({"windows": true, "programs": []}));
        }
        let entries: Vec<String> = recipes
            .iter()
            .map(|r| format!("{}|{}|{}", r.id, r.command, r.apt.as_deref().unwrap_or("-")))
            .collect();
        let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
        let said = self.guest_sh("programs.list", LIST, &entries, 60)?;
        let mut answer = data!({
            "windows": false, "system": "", "arch": "", "free_kb": 0, "busy": false,
            "apt": false, "reach": false, "lists": false,
            "adding": self.programs_job.as_ref().is_some_and(|job| !matches!(job.phase, "done" | "failed")),
        });
        let mut on: Map<String, Value> = Map::new();
        let mut can: Map<String, Value> = Map::new();
        for (key, value) in lines_of(&said) {
            match key.as_str() {
                "system" | "arch" => {
                    answer.insert(key, json!(value));
                }
                "free_kb" => {
                    answer.insert(key, json!(value.trim().parse::<u64>().unwrap_or(0)));
                }
                "busy" | "apt" | "reach" | "lists" => {
                    answer.insert(key, json!(value == "yes"));
                }
                "on" => {
                    let mut parts = value.splitn(3, ' ');
                    let id = parts.next().unwrap_or_default().to_owned();
                    let mine = parts.next() == Some("yes");
                    let version = plain_version(parts.next().unwrap_or_default().trim());
                    on.insert(id, json!({"mine": mine, "version": version}));
                }
                "can" => {
                    let parts: Vec<&str> = value.split(' ').collect();
                    if let [id, version, files, bytes] = parts[..] {
                        can.insert(
                            id.to_owned(),
                            json!({
                                "version": plain_version(version),
                                "files": files.parse::<u64>().unwrap_or(0),
                                "bytes": bytes.parse::<u64>().unwrap_or(0),
                            }),
                        );
                    }
                }
                _ => {}
            }
        }
        let arch = answer
            .get("arch")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let apt = answer.get("apt").and_then(Value::as_bool).unwrap_or(false);
        let lists = answer
            .get("lists")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let programs: Vec<Value> = recipes
            .iter()
            .map(|recipe| {
                let mut row = recipe.row();
                if let Some(seen) = on.get(&recipe.id) {
                    row["on"] = json!(true);
                    row["mine"] = seen["mine"].clone();
                    row["version"] = seen["version"].clone();
                } else if let Some(release) = &recipe.file {
                    row["version"] = json!(release.version);
                    match release.builds.get(&arch) {
                        Some(build) => {
                            row["files"] = json!(1);
                            row["bytes"] = build.get("bytes").cloned().unwrap_or(json!(0));
                        }
                        None => row["cannot"] = json!("arch"),
                    }
                } else if let Some(seen) = can.get(&recipe.id) {
                    row["version"] = seen["version"].clone();
                    row["files"] = seen["files"].clone();
                    row["bytes"] = seen["bytes"].clone();
                } else {
                    row["cannot"] = json!(if !apt {
                        "system"
                    } else if !lists {
                        "lists"
                    } else {
                        "absent"
                    });
                }
                row
            })
            .collect();
        answer.insert("programs".into(), json!(programs));
        Ok(answer)
    }

    /// Start adding a program: its files fetched here, or, for apt with
    /// the internet in the guest, installed straight from there.
    pub(super) fn programs_add(&mut self, action: &Action) -> Result<Data, ProviderError> {
        if self
            .programs_job
            .as_ref()
            .is_some_and(|job| job.phase != "done" && job.phase != "failed")
        {
            return Err(ProviderError::new(reason!("host.programs_busy")));
        }
        let recipe = self.recipe(action)?;
        let listed = self.programs_list(false)?;
        let row = listed
            .get("programs")
            .and_then(Value::as_array)
            .and_then(|rows| rows.iter().find(|row| row["id"] == json!(recipe.id)))
            .cloned()
            .unwrap_or_default();
        if row["on"] == json!(true) {
            return Err(ProviderError::new(reason!(
                "host.program_there",
                program = recipe.name
            )));
        }
        if listed.get("busy").and_then(Value::as_bool) == Some(true) {
            return Err(ProviderError::new(reason!("host.programs_busy")));
        }
        let free_kb = listed.get("free_kb").and_then(Value::as_u64).unwrap_or(0);
        let reach = listed.get("reach").and_then(Value::as_bool) == Some(true);
        let arch = listed
            .get("arch")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let room = |bytes: u64| {
            if free_kb < room_needed_kb(bytes) {
                Err(ProviderError::new(reason!(
                    "host.no_room",
                    program = recipe.name
                )))
            } else {
                Ok(())
            }
        };

        let refresh = action.argument("refresh").and_then(Value::as_bool) == Some(true);
        let (how, version, files, indexes) = if let Some(release) = &recipe.file {
            let build = release.builds.get(&arch).ok_or_else(|| {
                ProviderError::new(reason!(
                    "host.program_no_build",
                    program = recipe.name,
                    arch = arch
                ))
            })?;
            let url = field(build, "url");
            let wanted = Wanted {
                name: url.rsplit('/').next().unwrap_or("program").to_owned(),
                url,
                bytes: build.get("bytes").and_then(Value::as_u64).unwrap_or(0),
                sha256: field(build, "sha256"),
            };
            room(wanted.bytes)?;
            ("file", release.version.clone(), vec![wanted], Vec::new())
        } else {
            match row["cannot"].as_str() {
                Some("system") => return Err(ProviderError::new(reason!("host.no_apt"))),
                // Refreshing the lists may be what brings it.
                Some("lists") if !reach && !refresh => {
                    return Err(ProviderError::new(reason!("host.old_list")));
                }
                Some("absent") if !reach && !refresh => {
                    return Err(ProviderError::new(reason!(
                        "host.program_not_in_system",
                        program = recipe.name
                    )));
                }
                _ => {}
            }
            if reach {
                ("apt", String::new(), Vec::new(), Vec::new())
            } else if refresh {
                let indexes = self.guest_indexes()?;
                let mut files: Vec<Wanted> = Vec::new();
                for index in &indexes {
                    if !files.iter().any(|f| f.name == index.release) {
                        files.push(Wanted {
                            url: format!("{}InRelease", index.base),
                            name: index.release.clone(),
                            bytes: 0,
                            sha256: String::new(),
                        });
                    }
                }
                if files.is_empty() {
                    return Err(ProviderError::new(reason!("host.old_list")));
                }
                ("lists", String::new(), files, indexes)
            } else {
                let files = self.debs_wanted(&recipe)?;
                room(files.iter().map(|f| f.bytes).sum())?;
                ("debs", String::new(), files, Vec::new())
            }
        };

        let mut job = Job {
            recipe,
            how,
            indexes,
            list_bytes: 0,
            free_kb,
            version,
            files,
            folder: self.workspace.join("programs"),
            fetch: Arc::default(),
            copied: 0,
            phase: "download",
            failed: None,
            said: String::new(),
            lines: Vec::new(),
        };
        if how == "apt" {
            self.programs_job = Some(job);
            self.programs_install()?;
        } else {
            start_fetch(&mut job)?;
            self.programs_job = Some(job);
        }
        self.programs_adding()
    }

    /// The `.deb` files a package needs, as the guest's lists say.
    fn debs_wanted(&mut self, recipe: &Recipe) -> Result<Vec<Wanted>, ProviderError> {
        let package = recipe.apt.clone().unwrap_or_default();
        let said = self.guest_sh("programs.add", WANTS, &[&package], 60)?;
        let files: Vec<Wanted> = lines_of(&said)
            .into_iter()
            .filter(|(key, _)| key == "file")
            .filter_map(|(_, value)| {
                let parts: Vec<&str> = value.split(' ').collect();
                let [url, bytes, sha256, name] = parts[..] else {
                    return None;
                };
                // A name from the guest becomes a path here and there:
                // nothing but a plain .deb file name.
                let safe = simple(name, "._+%-") && name.ends_with(".deb");
                (safe && sha256.len() == 64).then(|| Wanted {
                    url: url.to_owned(),
                    name: name.to_owned(),
                    bytes: bytes.parse().unwrap_or(0),
                    sha256: sha256.to_owned(),
                })
            })
            .collect();
        if files.is_empty() || !files.iter().all(|f| f.url.starts_with("http")) {
            return Err(ProviderError::new(reason!(
                "host.program_not_in_system",
                program = recipe.name
            )));
        }
        Ok(files)
    }

    /// The package indexes in the guest's lists, each with its release.
    fn guest_indexes(&mut self) -> Result<Vec<Index>, ProviderError> {
        let said = self.guest_sh("programs.add", INDEXES, &[], 60)?;
        Ok(indexes_of(&stdout_of(&said)))
    }

    /// The `InRelease` files are in: have the guest check them, and fetch
    /// the indexes that changed.
    fn lists_check(&mut self) -> Result<(), ProviderError> {
        let job = self.programs_job.as_ref().expect("a job");
        let entries: Vec<String> = job
            .indexes
            .iter()
            .map(|i| format!("{}|{}|{}", i.release, i.meta, i.target))
            .collect();
        let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
        let said = self.guest_sh("programs.add", LISTS_CHECK, &entries, 180)?;
        let job = self.programs_job.as_mut().expect("held above");
        if code_of(&said) != 0 {
            job.phase = "failed";
            job.failed = Some(reason!("host.list_unchecked"));
            return Ok(());
        }
        let mut files: Vec<Wanted> = Vec::new();
        for (key, value) in lines_of(&said) {
            let parts: Vec<&str> = value.split(' ').collect();
            let (true, [release, meta, sha, bytes, target]) = (key == "want", &parts[..]) else {
                continue;
            };
            if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let Some(index) = job
                .indexes
                .iter_mut()
                .find(|i| i.release == *release && i.meta == *meta && i.target == *target)
            else {
                continue;
            };
            index.sha256 = Some((*sha).to_owned());
            if files.iter().any(|f| f.name == *sha) {
                continue;
            }
            let folder = index.meta.rsplit_once('/').map_or("", |(dir, _)| dir);
            files.push(Wanted {
                url: if index.by_hash {
                    format!("{}{folder}/by-hash/SHA256/{sha}", index.base)
                } else {
                    format!("{}{}.xz", index.base, index.meta)
                },
                name: (*sha).to_owned(),
                bytes: bytes.parse().unwrap_or(0),
                sha256: (*sha).to_owned(),
            });
        }
        if files.is_empty() {
            return self.lists_apply();
        }
        job.how = "indexes";
        job.files = files;
        start_fetch(job)
    }

    /// Everything is in: swap the lists, then fetch what the program needs
    /// by the new ones.
    fn lists_apply(&mut self) -> Result<(), ProviderError> {
        let job = self.programs_job.as_ref().expect("a job");
        let mut entries: Vec<String> = Vec::new();
        for index in &job.indexes {
            let release = format!("R|{}", index.release);
            if !entries.contains(&release) {
                entries.push(release);
            }
        }
        entries.extend(
            job.indexes
                .iter()
                .filter_map(|i| i.sha256.as_ref().map(|sha| format!("I|{sha}|{}", i.target))),
        );
        let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
        let said = self.guest_sh("programs.add", LISTS_APPLY, &entries, 180)?;
        let job = self.programs_job.as_mut().expect("held above");
        if code_of(&said) != 0 {
            job.phase = "failed";
            job.failed = Some(reason!("host.list_not_refreshed"));
            return Ok(());
        }
        let (recipe, free_kb) = (job.recipe.clone(), job.free_kb);
        let wanted = self.debs_wanted(&recipe).and_then(|files| {
            if free_kb < room_needed_kb(files.iter().map(|f| f.bytes).sum()) {
                Err(ProviderError::new(reason!(
                    "host.no_room",
                    program = recipe.name
                )))
            } else {
                Ok(files)
            }
        });
        let job = self.programs_job.as_mut().expect("held above");
        match wanted {
            Ok(files) => {
                job.how = "debs";
                job.files = files;
                start_fetch(job)?;
            }
            Err(error) => {
                job.phase = "failed";
                job.failed = Some(
                    error
                        .reason
                        .unwrap_or_else(|| reason!("host.list_not_refreshed")),
                );
            }
        }
        Ok(())
    }

    /// Start the install inside, from what was copied in (or from apt).
    fn programs_install(&mut self) -> Result<(), ProviderError> {
        let Some(job) = self.programs_job.as_ref() else {
            return Ok(());
        };
        let units = job.recipe.keep_off.join(" ");
        let mut parameters: Vec<String> = vec![
            INSTALL.into(),
            job.how.into(),
            job.recipe.id.clone(),
            job.version.clone(),
            units,
        ];
        match job.how {
            "apt" => parameters.push(job.recipe.apt.clone().unwrap_or_default()),
            "debs" => {
                parameters.push(job.recipe.apt.clone().unwrap_or_default());
                parameters.extend(job.files.iter().map(|f| format!("{INCOMING}/{}", f.name)));
            }
            _ => {
                let release = job.recipe.file.as_ref().expect("a file recipe");
                parameters.push(format!("{INCOMING}/{}", job.files[0].name));
                parameters.push(release.inside.clone().unwrap_or_else(|| "-".into()));
                parameters.push(job.recipe.command.clone());
            }
        }
        let parameters: Vec<&str> = parameters.iter().map(String::as_str).collect();
        let said = self.guest_sh("programs.add", APPLY, &parameters, 30)?;
        let job = self.programs_job.as_mut().expect("checked above");
        match code_of(&said) {
            0 => job.phase = "install",
            3 => {
                job.phase = "failed";
                job.failed = Some(reason!("host.programs_busy"));
            }
            _ => {
                job.phase = "failed";
                job.failed = Some(reason!(
                    "host.program_install_failed",
                    program = job.recipe.name
                ));
            }
        }
        Ok(())
    }

    /// How adding is going, doing the next step that needs the guest: one
    /// file copied in, the install started, or it tried.
    pub(super) fn programs_adding(&mut self) -> Result<Data, ProviderError> {
        let Some(job) = self.programs_job.as_mut() else {
            return Ok(data!({"phase": "none"}));
        };
        if job.phase == "download" {
            let state = job.fetch.lock().expect("fetch state");
            if let Some(why) = &state.failed {
                job.failed = Some(why.clone());
                job.phase = "failed";
            } else if state.done {
                job.phase = "copy";
            }
        }
        if job.phase == "copy" {
            if job.copied == 0 {
                let arguments = [json!("-p"), json!(INCOMING)];
                self.with_agent("programs.add", |agent| {
                    agent.run("mkdir", &arguments, Some(Duration::from_secs(10)))
                })?;
            }
            let job = self.programs_job.as_mut().expect("held above");
            if job.copied < job.files.len() {
                let wanted = job.files[job.copied].clone();
                let local = job.folder.join(&wanted.name);
                let bytes = std::fs::read(&local)
                    .map_err(|e| ProviderError::new(reason!("host.download_failed", detail = e)))?;
                let target = format!("{INCOMING}/{}", wanted.name);
                let written =
                    self.with_agent("programs.add", |agent| agent.write_bytes(&target, &bytes));
                let job = self.programs_job.as_mut().expect("held above");
                match written {
                    Ok(count) if count == bytes.len() as u64 => {
                        job.copied += 1;
                        if matches!(job.how, "lists" | "indexes") {
                            job.list_bytes += count;
                        }
                    }
                    Ok(_) => {
                        job.phase = "failed";
                        job.failed = Some(reason!("host.copy_stopped"));
                    }
                    Err(error) => {
                        job.phase = "failed";
                        job.failed =
                            Some(error.reason.unwrap_or_else(|| reason!("host.copy_stopped")));
                    }
                }
            }
            let job = self.programs_job.as_ref().expect("held above");
            if job.phase == "copy" && job.copied == job.files.len() {
                let _ = std::fs::remove_dir_all(&job.folder);
                match job.how {
                    "lists" => self.lists_check()?,
                    "indexes" => self.lists_apply()?,
                    _ => self.programs_install()?,
                }
            }
        } else if job.phase == "install" {
            let said = self.guest_sh("programs.add", STATE, &[], 20)?;
            let lines = lines_of(&said);
            let state = lines
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.clone());
            let job = self.programs_job.as_mut().expect("held above");
            job.lines = lines
                .into_iter()
                .filter(|(k, _)| k == "line")
                .map(|(_, v)| v)
                .collect();
            match state.as_deref() {
                Some("done") => job.phase = "try",
                Some("running") => {}
                _ => {
                    job.phase = "failed";
                    job.failed = Some(reason!(
                        "host.program_install_failed",
                        program = job.recipe.name
                    ));
                }
            }
        }
        if self
            .programs_job
            .as_ref()
            .is_some_and(|job| job.phase == "try")
        {
            let words: Vec<String> = self
                .programs_job
                .as_ref()
                .expect("held")
                .recipe
                .try_it
                .clone();
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            let said = self.guest_sh("programs.add", TRY, &words, 20)?;
            let job = self.programs_job.as_mut().expect("held above");
            job.said = stdout_of(&said);
            if code_of(&said) == 0 {
                job.phase = "done";
                let (program, version) = (job.recipe.id.clone(), job.version.clone());
                self.emit(
                    "program.added",
                    data!({"program": program, "version": version}),
                );
            } else {
                job.phase = "failed";
                job.failed = Some(reason!(
                    "host.program_did_not_run",
                    program = job.recipe.name
                ));
            }
        }
        let job = self.programs_job.as_ref().expect("held above");
        let fetched = job.fetch.lock().expect("fetch state").fetched;
        let mut got: u64 = job.files[..fetched.min(job.files.len())]
            .iter()
            .map(|f| f.bytes)
            .sum();
        if let Some(next) = job.files.get(fetched) {
            got += std::fs::metadata(job.folder.join(format!("{}.part", next.name)))
                .map_or(0, |m| m.len());
        }
        Ok(data!({
            "program": job.recipe.id, "name": job.recipe.name, "how": job.how,
            "phase": job.phase, "files": job.files.len(), "fetched": fetched,
            "bytes": job.files.iter().map(|f| f.bytes).sum::<u64>(), "got": got,
            "copied": job.copied, "said": job.said, "lines": job.lines,
            "refreshed": !job.indexes.is_empty(), "list_bytes": job.list_bytes,
            "failed": job.failed.as_ref().map(|why| json!(why)),
        }))
    }

    /// Stop adding, while it is still downloading or copying: once the
    /// install has started inside, it finishes.
    pub(super) fn programs_stop(&mut self) -> Result<Data, ProviderError> {
        if let Some(job) = self.programs_job.as_mut()
            && matches!(job.phase, "download" | "copy")
        {
            job.fetch.lock().expect("fetch state").stop = true;
            job.phase = "failed";
            job.failed = Some(reason!("host.program_stopped"));
            let _ = std::fs::remove_dir_all(&job.folder);
        }
        self.programs_job = None;
        Ok(data!({}))
    }

    /// Remove a program BeatBox added. One the system came with stays.
    pub(super) fn programs_remove(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let recipe = self.recipe(action)?;
        let listed = self.programs_list(false)?;
        let mine = listed
            .get("programs")
            .and_then(Value::as_array)
            .and_then(|rows| rows.iter().find(|row| row["id"] == json!(recipe.id)))
            .is_some_and(|row| row["mine"] == json!(true));
        if !mine {
            return Err(ProviderError::new(reason!(
                "host.not_added_by_beatbox",
                program = recipe.name
            )));
        }
        let mark = format!("/var/lib/beatbox/programs/{}", recipe.id);
        let said = match &recipe.apt {
            Some(package) => self.guest_sh("programs.remove", REMOVE, &[package, &mark], 300)?,
            None => self.guest_sh(
                "programs.remove",
                r#"rm -f "/usr/local/bin/$1" "$2""#,
                &[&recipe.command, &mark],
                20,
            )?,
        };
        if code_of(&said) != 0 {
            return Err(ProviderError::new(reason!(
                "host.program_not_removed",
                program = recipe.name
            )));
        }
        self.emit("program.removed", data!({"program": recipe.id}));
        Ok(data!({"program": recipe.id}))
    }

    /// Keep a recipe somebody wrote, beside the built-ins.
    pub(super) fn programs_save(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let given = action.argument("recipe").cloned().unwrap_or_default();
        let recipe = Recipe::parse(&given, true).map_err(ProviderError::new)?;
        if built_in().iter().any(|r| r.id == recipe.id) {
            return Err(ProviderError::new(reason!(
                "host.program_built_in",
                program = recipe.name
            )));
        }
        let folder = self
            .own_programs()
            .ok_or_else(|| ProviderError::new(reason!("host.bad_program", field = "folder")))?;
        let text = serde_json::to_string_pretty(&recipe.json()).expect("a recipe serialises");
        std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::write(folder.join(format!("{}.json", recipe.id)), text))
            .map_err(|e| ProviderError::new(reason!("host.bad_program", field = e)))?;
        Ok(data!({"program": recipe.id}))
    }

    /// Forget a recipe somebody wrote. What it added stays on the machines.
    pub(super) fn programs_forget(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let recipe = self.recipe(action)?;
        if !recipe.own {
            return Err(ProviderError::new(reason!(
                "host.program_built_in",
                program = recipe.name
            )));
        }
        if let Some(folder) = self.own_programs() {
            let _ = std::fs::remove_file(folder.join(format!("{}.json", recipe.id)));
        }
        Ok(data!({"program": recipe.id}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_reads() {
        let all = built_in();
        assert_eq!(all.len(), 16);
        let mailpit = all.iter().find(|r| r.id == "mailpit").expect("mailpit");
        assert!(
            mailpit
                .file
                .as_ref()
                .is_some_and(|f| f.builds.contains_key("x86_64"))
        );
    }

    #[test]
    fn indexes_name_their_release_and_drop_the_odd() {
        let said = "http://pt.archive.ubuntu.com/ubuntu/dists/noble-updates/ \
/var/lib/apt/lists/pt.archive.ubuntu.com_ubuntu_dists_noble-updates_main_binary-amd64_Packages \
main/binary-amd64/Packages yes
file:/cdrom/dists/noble/ /var/lib/apt/lists/_cdrom_dists_noble_main_binary-amd64_Packages main/binary-amd64/Packages no
http://x/dists/a/ /var/lib/apt/lists/../../etc/x_main_binary-amd64_Packages main/binary-amd64/Packages no
http://x/dists/a/ /var/lib/apt/lists/x_y main/binary-amd64/Packages no";
        let all = indexes_of(said);
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].release,
            "pt.archive.ubuntu.com_ubuntu_dists_noble-updates_InRelease"
        );
        assert!(all[0].by_hash);
    }

    #[cfg(unix)]
    #[test]
    fn every_guest_script_parses() {
        let scripts = [
            LIST,
            WANTS,
            INDEXES,
            LISTS_CHECK,
            LISTS_APPLY,
            APPLY,
            INSTALL,
            STATE,
            REMOVE,
            TRY,
        ];
        for script in scripts {
            let parsed = Command::new("sh")
                .args(["-n", "-c", script])
                .status()
                .expect("sh runs");
            assert!(parsed.success(), "does not parse:\n{script}");
        }
    }

    #[test]
    fn versions_are_said_plainly() {
        assert_eq!(plain_version("1:2.43.0-1ubuntu7.3"), "2.43.0");
        assert_eq!(
            plain_version("7.94+git20230807.3be01efb1+dfsg-3build2"),
            "7.94"
        );
        assert_eq!(plain_version("2:8.3+93ubuntu2"), "8.3");
        assert_eq!(plain_version("1.34.0"), "1.34.0");
    }

    #[test]
    fn a_recipe_that_would_run_something_else_is_refused() {
        let base = json!({"id": "x", "name": "X", "command": "x", "try": ["x"], "apt": "x"});
        assert!(Recipe::parse(&base, true).is_ok());
        for (key, value) in [
            ("command", json!("x; rm -rf /")),
            ("apt", json!("-oAPT::x")),
            ("id", json!("../x")),
        ] {
            let mut bad = base.clone();
            bad[key] = value;
            assert!(Recipe::parse(&bad, true).is_err(), "{key}");
        }
        let file = json!({"id": "y", "name": "Y", "command": "y", "try": ["y"],
            "file": {"x86_64": {"url": "http://y/y", "sha256": "0".repeat(64)}}});
        assert!(Recipe::parse(&file, true).is_err(), "plain http");
        let both = json!({"id": "z", "name": "Z", "command": "z", "try": ["z"], "apt": "z",
            "file": {"x86_64": {"url": "https://z/z", "sha256": "0".repeat(64)}}});
        assert!(Recipe::parse(&both, true).is_err(), "two ways");
    }
}
