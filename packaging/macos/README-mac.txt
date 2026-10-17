OsmolControl for macOS (test build)

Install (no admin rights needed)
1. Double-click OsmolControl-macOS.tar.gz and move the OsmolControl folder into your home
   folder, e.g. ~/Applications/OsmolControl.
2. Open Terminal once and run (replace the path if you chose another folder):
     xattr -dr com.apple.quarantine ~/Applications/OsmolControl
   This removes the "downloaded from the internet" flag from these files only.
3. Double-click "osmolcontrol". Allow Bluetooth and local network access when macOS asks.

Ride
- On a company-managed Mac whose firewall blocks incoming connections, choose
  Settings -> "Controller only": MyWhoosh pairs the trainer over Bluetooth itself and
  OsmolControl connects the controller.
- Elsewhere, "OsmolControl gears" makes OsmolControl the trainer with your own gear setup.
- Press "Start ride" and follow the steps on the Ride tab.

OsmolControl.command / bikectl are the command-line version of the same app.
