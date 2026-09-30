# USB audio hardware

Some tests need a real USB audio interface. A job reaches one only if its runner runs where the device is: on the
machine it is plugged into, or in a VM or container the device was passed through to. bana labels that runner so
a job can ask for it:

```yaml
  hardware:
    runs-on: [self-hosted, example-linux, usb-1c75-af70]   # this device, on any Linux machine that has it
  any-interface:
    runs-on: [self-hosted, example-macos, usb-audio]       # any Mac with a USB audio device
```

`bana usb` lists what a machine has and the labels it gives. Labels are set when a runner registers. After you
plug a device in or out, `bana relabel` updates them through the API (it needs `gh auth login`). Only the
first runner on a machine gets the hardware labels, so two jobs never use one device at the same time.

## Where devices can reach a runner

| Runner | USB devices |
|---|---|
| macOS runner on a Mac | yes: it runs natively, so devices plugged into the Mac are its own |
| OrbStack or Lima machine on a Mac | no: they have no USB passthrough |
| Tart VM | no: Tart has no USB passthrough (openai/tart#139, "not possible at the moment") |
| Proxmox VM | yes, passed through (below) |
| Proxmox container | yes, with the host's `/dev/snd` (below) |

So on a Mac, hardware tests run as macOS jobs. Hardware tests for Linux run on Proxmox (or any Linux PC).

## Proxmox: a VM

Find the device on the Proxmox host, then give it to the VM by vendor and product id:

```sh
lsusb                                   # e.g. Bus 001 Device 004: ID 1c75:af70 Arturia MiniFuse 2
qm set <vmid> -usb0 host=1c75:af70      # add ,usb3=1 for a USB 3 device
```

With USB hotplug on (Proxmox's default) a running VM gets it at once; otherwise restart the VM. Proxmox
attaches the device again whenever it is plugged in, even into another port. In the VM, the kernel's
`snd-usb-audio` driver takes it. Debian's `-cloud` kernels and Ubuntu's virtual ones leave that driver out, so
`bana up` checks the running kernel: on Debian it installs the standard kernel (`linux-image-amd64`) and asks
you to reboot, on Ubuntu it installs `linux-modules-extra-<release>` and loads the driver. (`--no-usb` skips
this.) Check with `bana usb`, and after a reboot, `bana relabel`.

A VM owns the device while it runs: the host and other VMs cannot use it. This is the simpler and more
isolated choice.

## Proxmox: a container

A container shares the host's kernel, so the host's driver handles the device and the container gets its device
nodes. In `/etc/pve/lxc/<ctid>.conf` on the host:

```
lxc.cgroup2.devices.allow: c 116:* rwm
lxc.mount.entry: /dev/snd dev/snd none bind,optional,create=dir
```

Restart the container. This gives it every sound card on the host, so keep the host itself from using them (a
Proxmox host normally has no desktop or audio server).

In an unprivileged container the nodes belong to an unmapped group and the runner user cannot open them. Either
make the container privileged, or on the host add a udev rule such as
`SUBSYSTEM=="sound", MODE="0666"` in `/etc/udev/rules.d/99-snd.rules` (then `udevadm control --reload` and
replug). bana adds its runner user to the `audio` group, which is enough in a privileged container.

Card numbers can change when a device is replugged. bana goes by vendor and product id, so the labels stay the
same.

## macOS VMs with USB passthrough (not built)

macOS 27 added USB passthrough to Apple's Virtualization framework (`VZUSBPassthroughDevice`). A VM host using
it needs a restricted entitlement, `com.apple.developer.accessory-access.usb`, and Apple grants that only
through a paid Apple Developer membership (a development certificate and a provisioning profile). It also needs
a sandboxed host app, macOS 27 with its SDK, and a one-time manual assignment of each device in the menu bar.
[beriberikix/usb-macos-vm](https://github.com/beriberikix/usb-macos-vm) shows a working build. Tart and UTM
do not offer it yet: a Tart fork could not carry the entitlement without its builder's own paid account, and
UTM's pull requests for it (#7877, #7635) are not merged.

Without a paid membership this is not available, and the native macOS runner already gives Mac jobs real
devices. It would matter only for running hardware tests in throwaway macOS VMs.
