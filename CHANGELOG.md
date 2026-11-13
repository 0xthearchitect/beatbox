# Changelog

## 0.1.1 (2026-11-13)

Fixes.

### Machines

- A long status message no longer pushes a machine's buttons onto a
  second row.
- A command that runs past its time limit and keeps going says "Still
  running", not that it failed. While a command runs, the page says which
  one.

### Networks

- The "Exposed servers" template uses English names when BeatBox is in
  English.
- The file picker no longer shows the top of the disk twice in its path.

### Servers

- A machine switched off and on, or gone back to a save point, while its
  server page was closed no longer leaves the services saying "Did not
  start". They say they stopped with their machine.
- A server page that has just opened no longer flashes "Off", "None" or a
  missing machine before it has checked.
- Configuration shows your kinds-of-service folder as `~/…` instead of
  your full home path.

## 0.1.0 (2026-11-05)

The first release.

### Machines

- Boot an operating system from an installer disc you already have, and
  install it on a disk of its own.
- Each machine runs in its own window, with hardware acceleration where
  the computer has it.
- Freeze a machine, take save points and go back to them, or throw it
  away.
- Machines can keep running after you close BeatBox, if you choose so in
  Settings.
- The disk you start from is never written to.

### Networks

- Routers, switches, VLANs and NAT, with full machines and light PCs and
  servers of a megabyte or two.
- A firewall with rules read top to bottom, between VLANs, groups,
  machines and the internet.
- Names and addresses for the network (DNS and DHCP), and IPv6.
- Every conversation, the way it went and what the firewall did to it, and
  captures per cable.
- Save a network to a file, or start one from a template.

### Servers

- A website, a site behind nginx, a PostgreSQL database, an email inbox and
  names for the network, run inside a machine.
- BeatBox checks what the machine needs first, and says what to install.
- Describe your own services.

### Everywhere

- Linux (`.deb`, `.rpm`, AppImage) and Windows 10 and 11.
- English and Portuguese.
