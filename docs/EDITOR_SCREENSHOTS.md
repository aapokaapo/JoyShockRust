# Profile editor screenshots

These screenshots show the GTK 4 and libadwaita profile editor running on
Wayland in dark mode with an example profile. The live previews use
synthetic controller readings.

## Buttons

Select a physical control to edit its bindings. This profile maps Cross
to Space on tap and Left Shift on hold. [DualSense SVG](../crates/joyshockrust-editor/assets/dualsense.svg)

![Buttons tab showing the DualSense layout, Xbox Series virtual gamepad selection and Cross keyboard bindings](screenshots/editor-buttons.png)

## Sticks

Choose a stick mode and compare the gray physical input with the blue
response. The right stick in this example uses Flick Stick.

![Sticks tab showing the physical and adjusted Flick Stick positions on one diagram](screenshots/editor-sticks.png)

![Left-stick response curve showing the gray physical travel point and blue adjusted travel point](screenshots/editor-stick-response.png)

## Touchpad

Choose a grid region to configure separate touch and click actions. The
preview uses the upper half of the same DualSense SVG, highlights the selected
region on the touchpad, and shows each finger as a dot. Changing the
number of rows or columns updates the grid immediately.

![Touchpad tab showing two finger positions on a DualSense-shaped pad with a two-by-two grid and upper controller outline](screenshots/editor-touchpad.png)

## Gyro

Choose the aim mode and activation, view the calibrated IMU readings, and
adjust horizontal and vertical sensitivity. The three angular velocity
sliders show signed movement with zero at the center.

![Gyro tab showing negative X, centered Y and positive Z angular velocity, with aim mode and activation controls](screenshots/editor-gyro.png)

## Gyro response

Compare the live input speed with the response under the selected
threshold and acceleration settings.

![Gyro response chart showing the gray input speed point and blue accelerated speed point](screenshots/editor-gyro-response.png)

## Advanced TOML

Open **Advanced** to edit the profile directly in TOML.

![Advanced tab showing the example profile as editable TOML](screenshots/editor-advanced.png)
