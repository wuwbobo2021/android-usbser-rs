## Testing

- Make sure the JDK, Android SDK and NDK is installed and their environment variables are configured.
- Make sure the Rust target `aarch64-linux-android` is installed.
- Install `cargo-apk` and make sure the release keystore is configured.
- Run `cargo apk build -r` under this directory, then do `adb install -r ../target/release/apk/android_usb_cdc_test.apk`.
- Connect the Android device to the PC via USB, then configure the adbd TCP port by `adb tcpip 5555`.
- Check the Android device's IP address and connect to it from the host PC: `adb connect <device_ip>:5555`.
- Run `adb logcat android_usb_cdc_test:D '*:S'` On PC for tracing. (try `adb shell` and run logcat in the shell if this cannot work)
- Connect your USB CDC-ACM serial adapter to the Android device, connect GND, Tx and Rx to another serial adapter at the PC side.
- Start the installed Android "App" (`android_usb_cdc_test`).
- On PC, find the PC side serial adapter's name, open the serial terminal tool and set the right port, set `Baudrate` to 115200, make sure `Parity` is None, `Data Bits` is 8, and `Stop Bits` is 1 (these are initial parameters), then open the port.
- Serial data sent from the PC should be valid strings ending with `\n`. Commands like `conf 9600,N,8,1`, `rts 0`, `rts 1` will be executed, and others will be sent back (in upper case) for verification.
