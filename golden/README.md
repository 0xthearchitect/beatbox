# golden/

The reference BeatBox's engine, contract and API are held to: what they
said and did, recorded deterministically (a fixed clock, fixed ids, a fixed
UUID, no real QEMU, and nothing under `~/.beatbox` touched).

**These change only in a commit that says why.** They are the reference, not
a snapshot to refresh whenever a test disagrees with it. Re-recording one to
make a test pass is how behaviour drifts without anyone deciding it should.

Two placeholders: `{DIR}` is the temporary directory every path lived under
(put your own in its place), and `{UUID}` is the SMBIOS UUID each machine
draws fresh (compare it by shape: a v4 UUID).

| File | What it holds | Read by |
|---|---|---|
| `qemu/machine.json` | `machine.build_command` for six machines under four hosts (KVM, TCG, WHPX, TCG with a chosen CPU; UNIX sockets or loopback ports), and the reasons for six it refuses | `beatbox-isolation`, R2 |
| `qemu/install.json` | `machine.build_install_command` for three installs, and what it refuses | `beatbox-isolation`, R2 |
| `qemu/qemu-img.json` | the `qemu-img` argv for a disk, an overlay and reading save points; `-display help` read as windowed or not; the install hint for seven kinds of `/etc/os-release` | `beatbox-isolation`, R2 |
| `host/spec.json` | a host sandbox's configuration turned into a `MachineSpec`, its argv and its capabilities, for ten configurations | `beatbox-host`, R2 |
| `contract/types.json` | every type in `contract.json` as `wire()` sends it, once with every optional filled and once with none | `beatbox-contract`, R0 |
| `contract/envelopes.json` | what the door says to an unknown action, to every action given a field it does not take, and to every action missing the ones it needs | `beatbox-api`, R3 |
| `engine/walk.json`, `walk-crash.json` | the Python's `scripts/beatbox_engine_demo.py` as data: every event, every outcome, every state | `beatbox-engine`, R1 |
| `api/walk.json` | the facade driven the way the page drives it, against `MockSandbox`: 33 requests and their envelopes, refusals and a lost sandbox included | `beatbox-contract` R0, `beatbox-api` R3 |
| `api/records.json` | the record `~/.beatbox/state/<id>.json` holds after every lifecycle change of that walk | `beatbox-api`, R3 |
