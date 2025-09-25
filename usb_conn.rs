use jni::objects::JObject;
use jni_min_helper::*;

use crate::Error;
use futures_lite::StreamExt;
use nusb::MaybeFuture;
use std::{io::ErrorKind, pin::Pin, task, time::Duration};

use crate::usb::{jerr, list_devices, DeviceInfo};

const USB_SERVICE: &str = "usb";
const ACTION_USB_DEVICE_ATTACHED: &str = "android.hardware.usb.action.USB_DEVICE_ATTACHED";
const ACTION_USB_DEVICE_DETACHED: &str = "android.hardware.usb.action.USB_DEVICE_DETACHED";
const EXTRA_DEVICE: &str = "device";
const ACTION_USB_PERMISSION: &str = "rust.android_usbser.USB_PERMISSION"; // custom
const EXTRA_PERMISSION_GRANTED: &str = "permission";

/// Gets a global reference of `android.hardware.usb.UsbManager`.
#[inline(always)]
pub(crate) fn usb_manager() -> Result<&'static jni::objects::JObject<'static>, Error> {
    use std::sync::OnceLock;
    static USB_MAN: OnceLock<jni::objects::GlobalRef> = OnceLock::new();
    if let Some(ref_man) = USB_MAN.get() {
        Ok(ref_man.as_obj())
    } else {
        let usb_man = get_usb_manager()?;
        let _ = USB_MAN.set(usb_man.clone());
        Ok(USB_MAN.get().unwrap().as_obj())
    }
}

fn get_usb_manager() -> Result<jni::objects::GlobalRef, Error> {
    jni_with_env(|env| {
        let context = android_context();
        let usb_service_id = USB_SERVICE.new_jobject(env)?;
        let usb_man = env
            .call_method(
                context,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&usb_service_id).into()],
            )
            .get_object(env)?;

        let result = if !usb_man.is_null() {
            Ok(env.new_global_ref(&usb_man)?)
        } else {
            Err(Error::new(
                ErrorKind::Unsupported,
                "USB system service not found",
            ))
        };
        Ok(result)
    })
    .map_err(jerr)?
}

/// Checks if the Android context is an activity opened by an intent of
/// `android.hardware.usb.action.USB_DEVICE_ATTACHED`. If so, it takes the `DeviceInfo`
/// for the caller to open the device.
pub fn check_attached_intent() -> Result<DeviceInfo, Error> {
    // Note: `getIntent()` and `setIntent()` are functions of `Activity` (not `Context`)
    let dev_info = jni_with_env(|env| {
        let activity = android_context();

        // the Intent instance is taken from Activity by getIntent()
        let intent_startup = env
            .call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])
            .get_object(env)?;
        // checks if the action of current intent is ACTION_USB_DEVICE_ATTACHED
        let action_startup = BroadcastReceiver::get_intent_action(&intent_startup, env)?;
        if action_startup.trim() != ACTION_USB_DEVICE_ATTACHED {
            return Ok(Err(Error::from(ErrorKind::NotFound)));
        }
        Ok(get_extra_device(&intent_startup, env))
    })
    .map_err(jerr)??;
    if dev_info.check_connection() && dev_info.has_permission()? {
        Ok(dev_info)
    } else {
        Err(Error::from(ErrorKind::NotConnected))
    }
}

fn get_extra_device(intent: &JObject<'_>, env: &mut jni::JNIEnv<'_>) -> Result<DeviceInfo, Error> {
    let extra_device = EXTRA_DEVICE.new_jobject(env).map_err(jerr)?;
    let java_dev = env
        .call_method(
            intent,
            "getParcelableExtra",
            // TODO: this is deprecated in API 33 and above without the class parameter.
            "(Ljava/lang/String;)Landroid/os/Parcelable;",
            &[(&extra_device).into()],
        )
        .get_object(env)
        .map_err(jerr)?;

    if !java_dev.is_null() {
        DeviceInfo::build(env, &java_dev).map_err(jerr)
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            "Unexpected: the Intent has no EXTRA_DEVICE",
        ))
    }
}

/// Gets a watcher of device connection / disconnection events.
pub fn watch_devices() -> Result<HotplugWatch, Error> {
    BroadcastWaiter::build([ACTION_USB_DEVICE_ATTACHED, ACTION_USB_DEVICE_DETACHED])
        .map(|waiter| HotplugWatch { waiter })
        .map_err(jerr)
}

/// Stream of device connection / disconnection events.
#[derive(Debug)]
pub struct HotplugWatch {
    waiter: BroadcastWaiter,
}

/// Event returned from the `HotplugWatch` stream.
#[derive(Clone, Debug)]
pub enum HotplugEvent {
    Connected(DeviceInfo),
    Disconnected(DeviceInfo),
}

#[derive(Debug)]
struct HotplugWatchFuture<'a> {
    watch: &'a mut HotplugWatch,
}

impl HotplugWatch {
    /// Returns the amount of received events available for checking.
    pub fn count_available(&self) -> usize {
        self.waiter.count_received()
    }

    /// Takes the next received event if available. This shouldn't conflict
    /// with the asynchonous feature (which requires a mutable reference).
    pub fn take_next(&mut self) -> Option<HotplugEvent> {
        (self.count_available() > 0).then_some(())?;
        self.wait_blocking(Duration::from_millis(1))
    }

    /// Waits for receiving an event; returns directly if an event is available.
    /// Note: Waiting in the `android_main()` thread will prevent it from receiving.
    pub fn wait_blocking(&mut self, timeout: Duration) -> Option<HotplugEvent> {
        let fut = HotplugWatchFuture { watch: self };
        block_for_timeout(fut, timeout)
    }
}

impl futures_core::Stream for HotplugWatch {
    type Item = HotplugEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Option<Self::Item>> {
        // `BroadcastWaiter` implementation makes `Ready(None)` impossible here
        if let task::Poll::Ready(Some(intent)) = self.waiter.poll_next(cx) {
            let result = jni_with_env(|env| {
                let action = BroadcastWaiter::get_intent_action(&intent, env)?;
                Ok(match action.trim() {
                    ACTION_USB_DEVICE_ATTACHED => get_extra_device(intent.as_obj(), env)
                        .ok()
                        .map(HotplugEvent::Connected),
                    ACTION_USB_DEVICE_DETACHED => get_extra_device(intent.as_obj(), env)
                        .ok()
                        .map(HotplugEvent::Disconnected),
                    _ => None,
                })
            });
            if let Ok(Some(event)) = result {
                task::Poll::Ready(Some(event))
            } else {
                task::Poll::Pending
            }
        } else {
            task::Poll::Pending
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.waiter.size_hint()
    }
}

impl<'a> std::future::Future for HotplugWatchFuture<'a> {
    type Output = HotplugEvent;
    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        if let task::Poll::Ready(Some(event)) = self.watch.poll_next(cx) {
            task::Poll::Ready(event)
        } else {
            task::Poll::Pending
        }
    }
}

impl DeviceInfo {
    /// Returns true if the caller has permission to access the device.
    pub fn has_permission(&self) -> Result<bool, Error> {
        let usb_man = usb_manager()?;
        jni_with_env(|env| {
            env.call_method(
                usb_man,
                "hasPermission",
                "(Landroid/hardware/usb/UsbDevice;)Z",
                &[self.internal.as_obj().into()],
            )
            .get_boolean()
        })
        .map_err(jerr)
    }

    /// Checks if the device is still in the list of connected devices.
    /// Note: The implementation can be optimized.
    #[inline(always)]
    pub fn check_connection(&self) -> bool {
        let vec_dev = list_devices().unwrap_or_default(); // heavy
        vec_dev.into_iter().any(|ref d| d == self)
    }

    /// Performs a permission request for the device.
    ///
    /// Returns `Ok(None)` if the permission is already granted. Otherwise it returns a
    /// `PermissionRequest` handler.
    ///
    /// The activity might be paused by `requestPermission()` here, but resumed on receving result.
    /// The state of `PermissionRequest` can be checked on `android_activity::MainEvent::Resume`,
    /// Otherwise block in a background thread (it wouldn't be paused/resumed automatically).
    pub fn request_permission(&self) -> Result<Option<PermissionRequest>, Error> {
        if !self.check_connection() {
            return Err(Error::from(ErrorKind::NotConnected));
        }
        if self.has_permission()? {
            return Ok(None);
        }

        let usb_man = usb_manager()?;
        jni_with_env(|env| {
            let context = android_context();

            let str_perm = ACTION_USB_PERMISSION.new_jobject(env)?;
            let intent = env
                .new_object(
                    "android/content/Intent",
                    "(Ljava/lang/String;)V",
                    &[(&str_perm).into()],
                )
                .auto_local(env)?;

            let flags = if android_api_level() < 31 {
                0 // should it be FLAG_IMMUTABLE since API 23?
            } else {
                0x02000000 // FLAG_MUTABLE (since API 31, Android 12)
            };
            let pending = env
                .call_static_method(
                    "android/app/PendingIntent",
                    "getBroadcast",
                    "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                    &[context.into(), 0_i32.into(), (&intent).into(), flags.into()],
                )
                .get_object(env)?;

            env.call_method(
                usb_man,
                "requestPermission",
                "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
                &[(&self.internal.as_obj()).into(), (&pending).into()],
            )
            .clear_ex()?;

            Ok(())
        })
        .map_err(jerr)?;

        if self.has_permission()? {
            return Ok(None); // almost impossible
        }
        BroadcastWaiter::build([ACTION_USB_PERMISSION])
            .map(|waiter| {
                Some(PermissionRequest {
                    dev_info: self.clone(),
                    waiter,
                })
            })
            .map_err(jerr)
    }

    /// Opens the device. Returns error `PermissionDenied` if the permission is not granted.
    pub fn open_device(&self) -> Result<nusb::Device, Error> {
        if !self.check_connection() {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "the device has been disconnected",
            ));
        }
        if !self.has_permission()? {
            return Err(Error::from(ErrorKind::PermissionDenied));
        }

        jni_with_env(|env| {
            let usb_man = match usb_manager() {
                Ok(man) => man,
                Err(e) => return Ok(Err(e)),
            };
            // Another thread executing `open_device` will block here, until the guard
            // for the current thread is dropped after `LinuxDevice::create_inner`.
            let _guard = env.lock_obj(usb_man).unwrap();

            let conn = env
                .call_method(
                    usb_man,
                    "openDevice",
                    "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
                    &[(&self.internal).into()],
                )
                .get_object(env)?;
            if conn.is_null() {
                return Ok(Err(Error::new(
                    ErrorKind::NotFound,
                    "`UsbManager.openDevice()` failed`",
                )));
            }
            let raw_fd = env
                .call_method(&conn, "getFileDescriptor", "()I", &[])
                .get_int()?;

            // Safety: `close()` is not called automatically when the JNI `AutoLocal` of `conn`
            // and the corresponding Java object is destroyed. (check `UsbDeviceConnection` source)
            use std::os::fd::*;
            log::debug!("Wrapping fd {raw_fd} as usbfs device");
            let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd as RawFd) };
            Ok(nusb::Device::from_fd(owned_fd)
                .wait()
                .map_err(|_| Error::from(ErrorKind::Other)))
        })
        .map_err(jerr)?
    }
}

/// Represents an ongoing permission request.
#[derive(Debug)]
pub struct PermissionRequest {
    dev_info: DeviceInfo,
    waiter: BroadcastWaiter,
}

impl PermissionRequest {
    /// Returns a reference of the associated `DeviceInfo` which can be cloned.
    pub fn device_info(&self) -> &DeviceInfo {
        &self.dev_info
    }

    /// Checks if the request has completed.
    pub fn responsed(&self) -> bool {
        self.waiter.count_received() > 0
    }

    /// Takes the `EXTRA_PERMISSION_GRANTED` extra from the received result.
    /// This can be called *after* `responsed()` returned true.
    pub fn take_response(self) -> Option<bool> {
        self.responsed().then_some(())?;
        block_for_timeout(self, Duration::from_millis(10))
    }

    /// Blocking permission request. Returns directly if the permission is already granted.
    /// Note: Blocking the `android_main()` thread will prevent it from receiving the result.
    pub fn wait_blocking(self, timeout: Duration) -> Result<bool, Error> {
        block_for_timeout(self, timeout).ok_or(Error::from(ErrorKind::TimedOut))
    }
}

impl std::future::Future for PermissionRequest {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        // `BroadcastWaiter` implementation makes `Ready(None)` impossible here
        if let task::Poll::Ready(Some(intent)) = self.waiter.poll_next(cx) {
            let result = jni_with_env(|env| {
                let Ok(dev_info) = get_extra_device(intent.as_obj(), env) else {
                    return Ok(None);
                };
                if dev_info == self.dev_info {
                    let extra_name = EXTRA_PERMISSION_GRANTED.new_jobject(env)?;
                    let granted = env
                        .call_method(
                            &intent,
                            "getBooleanExtra",
                            "(Ljava/lang/String;Z)Z",
                            &[(&extra_name).into(), false.into()],
                        )
                        .get_boolean()
                        .unwrap_or(false);
                    let _ = self.waiter.receiver().unregister();
                    Ok(Some(granted))
                } else {
                    Ok(None)
                }
            });
            if let Ok(Some(granted)) = result {
                task::Poll::Ready(granted)
            } else if let Ok(None) = result {
                task::Poll::Pending
            } else {
                task::Poll::Ready(false)
            }
        } else {
            task::Poll::Pending
        }
    }
}
