# Changes

## 0.2.3
* Bump `jni-min-helper` to 0.3. Threatened by the UB found in <https://github.com/wuwbobo2021/android-bluetooth-serial-rs/issues/2>.
* `check_attached_intent()` no longer returns the device if the permission is lost.
* Added default `serialport` feature which can be turned off (this choice will be removed in 0.3.0, see <https://github.com/kevinmehall/nusb/pull/150>).

## 0.2.2
* Fixed support for newest Android versions: `check_attached_intent()` does not work, `PermissionRequest` never returns the result of being permitted, both are caused by the bad implementation of `PartialEq` for `DeviceInfo`.
* Added `UsbSerial` trait to prepare for driver implementations of non-CDC serial adapters.
* The serial handler can be turned into `nusb` transfer queues for asynchronous operations, this can be done after serial configuration.

## 0.2.1
* Added `HotplugWatch`;
* `DeviceInfo::request_permission()` now returns a result of `Option<PermissionRequest>` instead of `()`.

## 0.2.0
* Switched to `nusb` for USB data transfering, instead of calling Java methods for reading and writing.
* `serialport` became a required dependency; the optional feature for `rusb` (which may not work on some Android devices) is removed.

## 0.1.1
* Fixed doc.rs build problem.

## 0.1.0
* Initial release.
