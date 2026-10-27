//! `network.probe`: from inside this guest, reach a service the way its user
//! would (a port, a web page, a DNS name) and say what came back and how
//! long it took. Only a machine on that network can answer "can it open my
//! site?".
//!
//! **The scripts are constants; what changes arrives as checked arguments**
//! (IPv4, port, DNS labels). PostgreSQL logins travel as hex-encoded JSON,
//! never as shell code, and skip process events so credentials stay private.

use std::net::Ipv4Addr;

use beatbox_engine::{Data, Reason, data, reason};
use serde_json::Value;

/// `<ms> <exit>`: 0 when something accepted the connection.
pub const LINUX_TCP: &str = r#"s=$(date +%s%N)
timeout 5 bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$1" "$2" 2>/dev/null
r=$?
echo "$(( ($(date +%s%N) - s) / 1000000 )) $r""#;

/// `<ms> <status line>`, the line empty when nothing answered.
pub const LINUX_HTTP: &str = r#"s=$(date +%s%N)
l=$(timeout 5 bash -c 'exec 3<>"/dev/tcp/$0/$1" || exit 1
printf "GET / HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n" "$0" >&3
IFS= read -r l <&3
printf "%s" "$l"' "$1" "$2" 2>/dev/null)
echo "$(( ($(date +%s%N) - s) / 1000000 )) ${l%$'\r'}""#;

/// `<ms> <rcode> [a.b.c.d]`, or `<ms> timeout`. One A question, built
/// byte by byte and sent as one datagram; the reply read back as numbers.
pub const LINUX_DNS: &str = r#"s=$(date +%s%N)
ms() { echo $(( ($(date +%s%N) - s) / 1000000 )); }
q=$(mktemp) || exit 1
{
  printf '\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00'
  IFS=.
  for l in $3; do printf "\\x$(printf %02x ${#l})%s" "$l"; done
  IFS=' '
  printf '\x00\x00\x01\x00\x01'
} > "$q"
if ! exec 3<>"/dev/udp/$1/$2"; then rm -f "$q"; echo "$(ms) timeout"; exit 0; fi
cat "$q" >&3
rm -f "$q"
r=$(timeout 3 dd bs=512 count=1 <&3 2>/dev/null | od -An -tu1 -v)
set -- $r
if [ $# -lt 12 ]; then echo "$(ms) timeout"; exit 0; fi
code=$(( $4 & 15 ))
count=$(( $7 * 256 + $8 ))
if [ "$count" -gt 0 ]; then
  n=$#
  eval "a=\${$((n - 3))}.\${$((n - 2))}.\${$((n - 1))}.\${$n}"
  echo "$(ms) $code $a"
else
  echo "$(ms) $code"
fi"#;

/// The same three, in PowerShell 5 (Windows' own).
pub const WINDOWS_TCP: &str = r#"& { param($a, $p) $w = [Diagnostics.Stopwatch]::StartNew(); $c = New-Object Net.Sockets.TcpClient; try { $ok = $c.ConnectAsync($a, [int]$p).Wait(5000) } catch { $ok = $false }; $c.Close(); "$($w.ElapsedMilliseconds) $(if ($ok) { 0 } else { 1 })" }"#;
/// See [`WINDOWS_TCP`].
pub const WINDOWS_HTTP: &str = r#"& { param($a, $p) $w = [Diagnostics.Stopwatch]::StartNew(); try { $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 ("http://" + $a + ":" + $p + "/"); "$($w.ElapsedMilliseconds) HTTP/1.1 $($r.StatusCode) $($r.StatusDescription)" } catch { if ($_.Exception.Response) { "$($w.ElapsedMilliseconds) HTTP/1.1 $([int]$_.Exception.Response.StatusCode)" } else { "$($w.ElapsedMilliseconds) " } } }"#;
/// See [`WINDOWS_TCP`].
pub const WINDOWS_DNS: &str = r#"& { param($a, $p, $n) $w = [Diagnostics.Stopwatch]::StartNew(); try { $r = Resolve-DnsName -Name $n -Server $a -Type A -DnsOnly -QuickTimeout -ErrorAction Stop; $ip = ($r | Where-Object { $_.IPAddress } | Select-Object -First 1).IPAddress; "$($w.ElapsedMilliseconds) 0 $ip" } catch { if ($_.Exception.Message -match 'does not exist') { "$($w.ElapsedMilliseconds) 3" } else { "$($w.ElapsedMilliseconds) timeout" } } }"#;

/// `<ms> <greeting>` from an SMTP service in a Linux guest.
pub const LINUX_SMTP: &str = r#"s=$(date +%s%N)
l=$(timeout 5 bash -c 'exec 3<>"/dev/tcp/$0/$1" || exit 1; IFS= read -r l <&3; printf "%s" "$l"' "$1" "$2" 2>/dev/null)
echo "$(( ($(date +%s%N) - s) / 1000000 )) ${l%$'\r'}""#;
/// The SMTP greeting probe in Windows PowerShell 5.
pub const WINDOWS_SMTP: &str = r#"& { param($a,$p) $w=[Diagnostics.Stopwatch]::StartNew();$c=New-Object Net.Sockets.TcpClient;try { if(!$c.ConnectAsync($a,[int]$p).Wait(5000)){throw 'timeout'};$s=$c.GetStream();$s.ReadTimeout=5000;$r=New-Object IO.StreamReader($s);$line=$r.ReadLine();"$($w.ElapsedMilliseconds) $line" } catch { "$($w.ElapsedMilliseconds) " } finally { $c.Close() } }"#;

/// What to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Does something accept a connection?
    Tcp,
    /// Does `GET /` answer, and with what?
    Http,
    /// What address does the DNS server there give a name?
    Dns,
    /// Validate a SMTP greeting, rather than only opening its port.
    Smtp,
    /// Authenticate and run a read-only SQL query from the chosen guest.
    Postgresql,
}

/// What a probe was asked, checked.
#[derive(Clone)]
pub struct Ask {
    /// What to ask.
    pub kind: Kind,
    /// Where.
    pub address: Ipv4Addr,
    /// On which port.
    pub port: u16,
    /// For DNS, the name.
    pub name: String,
    /// Never emitted as process events or included in result messages.
    credentials: Data,
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Ask")
            .field("kind", &self.kind)
            .field("address", &self.address)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

fn bad(field: &str) -> Reason {
    reason!("host.bad_probe", field = field)
}

/// A name a DNS question may carry, and nothing a shell could read as more.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

impl Ask {
    /// The probe `arguments` describe, or which of them is wrong.
    pub fn from(arguments: &Data) -> Result<Self, Reason> {
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
        };
        let kind = match text("kind") {
            "tcp" => Kind::Tcp,
            "http" => Kind::Http,
            "dns" => Kind::Dns,
            "smtp" => Kind::Smtp,
            "postgresql" => Kind::Postgresql,
            _ => return Err(bad("kind")),
        };
        let address = text("address").parse().map_err(|_| bad("address"))?;
        let port = arguments
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
            .ok_or_else(|| bad("port"))?;
        let name = text("name").trim_end_matches('.').to_ascii_lowercase();
        if kind == Kind::Dns && !is_name(&name) {
            return Err(bad("name"));
        }
        let mut credentials = Data::new();
        if kind == Kind::Postgresql {
            for field in ["username", "database", "password"] {
                let value = arguments
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if value.len() > 1024
                    || value.contains('\0')
                    || (field != "password" && value.trim().is_empty())
                {
                    return Err(bad(field));
                }
                credentials.insert(field.into(), value.into());
            }
        }
        Ok(Self {
            kind,
            address,
            port,
            name,
            credentials,
        })
    }

    /// The program and its arguments, for a guest of `windows` or not.
    pub fn argv(&self, windows: bool, powershell: &str) -> (String, Vec<String>) {
        let (address, port) = (self.address.to_string(), self.port.to_string());
        if self.kind == Kind::Postgresql {
            let mut config = self.credentials.clone();
            config.insert("address".into(), address.into());
            config.insert("port".into(), self.port.into());
            let hex: String = serde_json::to_vec(&config)
                .expect("JSON object")
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            return if windows {
                let script = format!("& {{\n{}\n}} '{hex}'", include_str!("postgresql_probe.ps1"));
                (
                    powershell.to_owned(),
                    vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        script,
                    ],
                )
            } else {
                (
                    "python3".into(),
                    vec!["-c".into(), include_str!("postgresql_probe.py").into(), hex],
                )
            };
        }
        let script = match (windows, self.kind) {
            (false, Kind::Tcp) => LINUX_TCP,
            (false, Kind::Http) => LINUX_HTTP,
            (false, Kind::Dns) => LINUX_DNS,
            (true, Kind::Tcp) => WINDOWS_TCP,
            (true, Kind::Http) => WINDOWS_HTTP,
            (true, Kind::Dns) => WINDOWS_DNS,
            (false, Kind::Smtp) => LINUX_SMTP,
            (true, Kind::Smtp) => WINDOWS_SMTP,
            (_, Kind::Postgresql) => unreachable!("handled above"),
        };
        let mut words: Vec<String> = if windows {
            ["-NoProfile", "-NonInteractive", "-Command", script]
                .map(str::to_owned)
                .to_vec()
        } else {
            ["-c", script, "bash"].map(str::to_owned).to_vec()
        };
        words.extend([address, port]);
        if self.kind == Kind::Dns {
            words.push(self.name.clone());
        }
        let program = if windows { powershell } else { "/bin/bash" };
        (program.to_owned(), words)
    }

    /// What the script printed, as the answer: `ok`, `ms`, `said` and, for
    /// DNS, `answer`.
    pub fn read(&self, stdout: &str) -> Data {
        let line = stdout
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim();
        let (ms, rest) = line.split_once(' ').unwrap_or((line, ""));
        let ms = ms.parse::<u64>().unwrap_or(0);
        let rest = rest.trim();
        match self.kind {
            Kind::Postgresql => {
                let said = match rest {
                    "query_ok"
                    | "authentication_failed"
                    | "database_missing"
                    | "query_failed"
                    | "unsupported_authentication"
                    | "tls_error"
                    | "tls_required"
                    | "connection_failed" => rest,
                    _ => "protocol_error",
                };
                data!({"ok": said == "query_ok", "ms": ms, "said": said})
            }
            Kind::Smtp => data!({"ok": rest.starts_with("220 "), "ms": ms, "said": rest}),
            Kind::Tcp => {
                data!({"ok": rest == "0", "ms": ms, "said": if rest == "0" { "connected" } else { "refused" }})
            }
            Kind::Http => {
                // "HTTP/1.1 200 OK" -> "200 OK"
                let status = rest.split_once(' ').map_or("", |(_, status)| status).trim();
                let code = status.split(' ').next().unwrap_or_default();
                let ok = code.len() == 3 && (code.starts_with('2') || code.starts_with('3'));
                data!({"ok": ok, "ms": ms, "said": status})
            }
            Kind::Dns => {
                let mut words = rest.split_whitespace();
                let code = words.next().unwrap_or("timeout");
                let answer = words.next().filter(|a| a.parse::<Ipv4Addr>().is_ok());
                let said = match code {
                    "0" if answer.is_some() => "answered",
                    "0" => "no address",
                    "3" => "no such name",
                    "timeout" => "no answer",
                    _ => "refused",
                };
                data!({"ok": answer.is_some(), "ms": ms, "said": said, "answer": answer})
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(arguments: Data) -> Result<Ask, Reason> {
        Ask::from(&arguments)
    }

    #[test]
    fn only_three_shapes_get_through() {
        assert!(ask(data!({"kind": "tcp", "address": "10.50.20.11", "port": 8000})).is_ok());
        for (arguments, field) in [
            (
                data!({"kind": "ftp", "address": "10.0.0.1", "port": 21}),
                "kind",
            ),
            (
                data!({"kind": "tcp", "address": "10.0.0.1; reboot", "port": 1}),
                "address",
            ),
            (
                data!({"kind": "tcp", "address": "10.0.0.1", "port": 0}),
                "port",
            ),
            (
                data!({"kind": "dns", "address": "10.0.0.1", "port": 53, "name": "a b"}),
                "name",
            ),
            (
                data!({"kind": "dns", "address": "10.0.0.1", "port": 53, "name": "$(id).lab"}),
                "name",
            ),
        ] {
            assert_eq!(ask(arguments).unwrap_err().values["field"], field);
        }
    }

    #[test]
    fn database_credentials_are_literal_bounded_and_never_debugged_or_returned() {
        let probe = ask(data!({"kind":"postgresql", "address":"10.0.0.1", "port":5432, "username":"user '; $(false)", "database":"app", "password":"private-pass ' ; $(id)"})).unwrap();
        assert!(!format!("{probe:?}").contains("private-pass"));
        let (_, words) = probe.argv(false, "ps");
        assert!(!words.last().unwrap().contains("private-pass"));
        let result = probe.read("5 authentication_failed");
        assert_eq!(result["ok"], false);
        assert_eq!(result["said"], "authentication_failed");
        assert_eq!(probe.read("5 query_ok")["ok"], true);
        assert_eq!(probe.read("oops secret-message")["said"], "protocol_error");
        assert!(ask(data!({"kind":"postgresql", "address":"10.0.0.1", "port":5432, "username":"x\0bad", "database":"app"})).is_err());
        let smtp = ask(data!({"kind":"smtp", "address":"10.0.0.1", "port":1025})).unwrap();
        assert_eq!(smtp.read("9 220 Mailpit ready")["ok"], true);
        assert_eq!(smtp.read("9 HTTP/1.1 200 OK")["ok"], false);
    }

    #[test]
    fn what_comes_back_is_read_in_words() {
        let http = ask(data!({"kind": "http", "address": "10.0.0.1", "port": 80})).unwrap();
        assert_eq!(http.read("14 HTTP/1.0 200 OK\n")["said"], "200 OK");
        assert_eq!(http.read("14 HTTP/1.0 200 OK\n")["ok"], true);
        assert_eq!(http.read("9 HTTP/1.1 404\n")["ok"], false);
        assert_eq!(http.read("5003 \n")["ok"], false);
        let dns =
            ask(data!({"kind": "dns", "address": "10.0.0.1", "port": 53, "name": "intranet.lab"}))
                .unwrap();
        assert_eq!(dns.read("3 0 10.50.20.11")["answer"], "10.50.20.11");
        assert_eq!(dns.read("3 3")["said"], "no such name");
        assert_eq!(dns.read("3001 timeout")["said"], "no answer");
        let tcp = ask(data!({"kind": "tcp", "address": "10.0.0.1", "port": 22})).unwrap();
        assert_eq!(tcp.read("2 0")["ok"], true);
        assert_eq!(tcp.read("2 1")["ok"], false);
    }

    #[test]
    fn the_name_is_one_argument_after_the_script() {
        let dns =
            ask(data!({"kind": "dns", "address": "10.0.0.1", "port": 53, "name": "Intranet.Lab."}))
                .unwrap();
        let (program, words) = dns.argv(false, "ps");
        assert_eq!(program, "/bin/bash");
        assert_eq!(words[1], LINUX_DNS);
        assert_eq!(words[3..], ["10.0.0.1", "53", "intranet.lab"]);
        let (program, words) = dns.argv(true, "ps.exe");
        assert_eq!(program, "ps.exe");
        assert_eq!(words[3], WINDOWS_DNS);
    }
}
