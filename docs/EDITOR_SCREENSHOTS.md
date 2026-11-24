# Profile editor screenshots

These screenshots show the GTK 4 and libadwaita profile editor running on
Wayland in dark mode with an example profile. The editor was captured with no
physical controller connected, so the live input and IMU previews are idle.

## Buttons

Select a physical control on the DualSense layout to edit its bindings. This
profile maps Cross to Space on tap and Left Shift on hold.

![Buttons tab showing the DualSense layout, Xbox Series virtual gamepad selection and Cross keyboard bindings](screenshots/editor-buttons.png)

## Sticks

Choose a stick mode and compare the physical and mapped input previews.
The right stick in this example uses Flick Stick.

![Sticks tab showing right-stick Flick Stick settings and paired physical and mapped input previews](screenshots/editor-sticks.png)

## Touchpad

Choose a grid region to configure separate touch and click actions.

![Touchpad tab showing a two-by-two grid and bindings for the selected region](screenshots/editor-touchpad.png)

## Gyro

Choose the aim mode and activation, view the calibrated IMU readings, and
adjust horizontal and vertical sensitivity.

![Gyro tab showing the aim mode selector, live calibrated IMU, gyro activation and axis sensitivity controls](screenshots/editor-gyro.png)

## Gyro response

Compare the configured acceleration curve with the linear response while
adjusting the acceleration settings.

![Gyro response section showing acceleration settings and a graph comparing configured and linear response](screenshots/editor-gyro-response.png)

## Advanced TOML

Open **Advanced** to edit the profile directly in TOML.

![Advanced tab showing the example profile as editable TOML](screenshots/editor-advanced.png)
