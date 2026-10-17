OsmolControl for Windows (test build)

Install
1. Unzip this folder anywhere, e.g. Documents\OsmolControl.
2. Double-click OsmolControl.exe.
   - SmartScreen may say "Windows protected your PC": click "More info" -> "Run anyway"
     (the test build is not code-signed).
   - When Windows Firewall asks, allow OsmolControl on Private networks.

Ride
1. Switch on the trainer and the controller. Do not pair the trainer to MyWhoosh over Bluetooth.
2. Pick your gears in the Gears tab and press "Start ride".
3. Follow the steps on the Ride tab to pair "OsmolControl" in MyWhoosh (Device Connection -> LAN),
   and turn Virtual Shifting off in MyWhoosh's settings.
4. Shift with the controller, the on-screen buttons or the Up/Down keys.
   "Overlay" opens a small always-on-top window with gear, power and cadence.
Closing the window stops the ride and resets the trainer.

MyWhoosh on the same laptop
OsmolControl and MyWhoosh can run side by side on one Windows PC: OsmolControl talks to the
trainer and the controller over Bluetooth, MyWhoosh connects to OsmolControl over the network.
- The PC must be connected to a network (Wi-Fi or cable), and that network should be set to
  Private (Settings -> Network & internet -> your network -> Network profile type).
- Start OsmolControl and press "Start ride" first, then start MyWhoosh.
- In MyWhoosh pair only "OsmolControl" under LAN. Do not pair the trainer or the controller over
  Bluetooth in MyWhoosh: one Bluetooth device accepts one app at a time.
- Ride on the big chainring: on the small one the trainer cannot reach high resistance in
  hard virtual gears.
- If MyWhoosh does not list OsmolControl, check the Log tab and allow OsmolControl in
  Windows Security -> Firewall & network protection -> Allow an app through firewall.

bikectl.exe / OsmolControl.bat are the command-line version of the same app.
