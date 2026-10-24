# MyWhoosh on Linux with OsmolControl

Run `packaging/linux/install.sh` as your normal user. It installs system packages (apt, dnf or
pacman), builds and installs OsmolControl, asks for MyWhoosh or MyWhoosh HD, and installs the game
in a pinned Proton prefix, with shortcuts and automatic game updates. MyWhoosh then reaches
OsmolControl as a Bluetooth trainer through `ble-bridge.py` (DirCon mode).

`mywhoosh.sh` and `ble-bridge.py` are shared with the public, OsmolControl-independent installer
[Parapeth962/mywhoosh-linux-installer](https://github.com/Parapeth962/mywhoosh-linux-installer),
whose README documents the design, commands and troubleshooting. Keep both copies in sync.

Ride: start **MyWhoosh** from the desktop or menu (OsmolControl opens too), press **Start ride**
in OsmolControl, then in MyWhoosh choose **Device Connection → BLE → Power Source →
OsmolControl**. With a heart rate strap (worn before Start ride), also choose
**Heart Rate → OsmolControl HR**. Turn MyWhoosh's Virtual Shifting off and ride the big chainring.
