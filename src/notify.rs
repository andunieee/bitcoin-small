//! system notifications: the desktop's notification service, android's notification manager
//! (through jni), or the browser's notifications. each one is best effort, a failure is only
//! logged.
//!
//! on android there is also the foreground service that keeps the app running in the background,
//! so the notifications keep coming.

/// show a notification. a later one with the same `tag` replaces it, where the system allows.
pub fn show(title: &str, body: &str, tag: &str) {
    imp::show(title.to_string(), body.to_string(), tag.to_string());
}

/// ask for the permission to show them, where one is needed and wasn't asked for yet. best called
/// right after the user did something, as browsers ignore it otherwise.
pub fn request_permission() {
    imp::request_permission();
}

/// keep the app running while it is in the background (`on`), or let go of that. only android
/// freezes apps, so only there it does anything.
pub fn set_background(on: bool) {
    imp::set_background(on);
}

/// call `f` with the `tag` of a notification when it is tapped, where the system tells us. on
/// android that includes the one the app was started by.
pub fn on_open(f: impl Fn(String) + Send + Sync + 'static) {
    imp::on_open(Box::new(f));
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
mod imp {
    pub fn set_background(_on: bool) {}

    pub fn on_open(_f: Box<dyn Fn(String) + Send + Sync>) {}

    pub fn show(title: String, body: String, _tag: String) {
        // it waits for the notification service, which is no reason to hold up the ui
        std::thread::spawn(move || {
            if let Err(err) = notify_rust::Notification::new()
                .appname("bitcoin smol")
                .summary(&title)
                .body(&body)
                .show()
            {
                log::warn!("failed to show a notification: {err}");
            }
        });
    }

    pub fn request_permission() {}
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use web_sys::{Notification, NotificationOptions, NotificationPermission};

    pub fn set_background(_on: bool) {}

    pub fn on_open(_f: Box<dyn Fn(String) + Send + Sync>) {}

    /// browsers without notifications have no `Notification` at all, and touching it would throw
    fn supported() -> bool {
        js_sys::Reflect::has(&js_sys::global(), &"Notification".into()).unwrap_or(false)
    }

    pub fn show(title: String, body: String, tag: String) {
        if !supported() || Notification::permission() != NotificationPermission::Granted {
            return;
        }
        let opts = NotificationOptions::new();
        opts.set_body(&body);
        opts.set_tag(&tag);
        if let Err(err) = Notification::new_with_options(&title, &opts) {
            log::warn!("failed to show a notification: {err:?}");
        }
    }

    pub fn request_permission() {
        if supported() && Notification::permission() == NotificationPermission::Default {
            let _ = Notification::request_permission();
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use jni::objects::{JClass, JObject, JString, JValue};
    use jni::{JNIEnv, JavaVM, NativeMethod};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// the service in android/src, which android/package.sh adds to the apk
    const SERVICE: &str = "cash.small.bitcoin.ForegroundService";
    /// whether the service is meant to be running
    static BACKGROUND: AtomicBool = AtomicBool::new(false);

    /// the activity in android/src, which hears the taps on notifications
    const ACTIVITY: &str = "cash.small.bitcoin.MainActivity";
    /// the extra of its intent holding the trade
    const TRADE: &str = "trade";
    static ON_OPEN: OnceLock<Box<dyn Fn(String) + Send + Sync>> = OnceLock::new();

    const CHANNEL: &str = "trades";
    /// NotificationManager.IMPORTANCE_HIGH, so it pops up
    const IMPORTANCE_HIGH: i32 = 4;
    /// Intent.FLAG_ACTIVITY_NEW_TASK | FLAG_ACTIVITY_CLEAR_TOP | FLAG_ACTIVITY_SINGLE_TOP, to
    /// reuse the activity that is running
    const INTENT_FLAGS: i32 = 0x1000_0000 | 0x0400_0000 | 0x2000_0000;
    /// PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT
    const PENDING_INTENT_FLAGS: i32 = 0x0400_0000 | 0x0800_0000;

    /// run `f` with the jni env of this thread and the activity
    fn with_activity(
        what: &str,
        f: impl for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> jni::errors::Result<()>,
    ) -> bool {
        let ctx = ndk_context::android_context();
        // SAFETY: android-activity sets these up before android_main, and they live as long as
        // the process. the activity is a global reference, which dropping a JObject leaves alone.
        let (vm, activity) = unsafe {
            (
                JavaVM::from_raw(ctx.vm().cast()),
                JObject::from_raw(ctx.context().cast()),
            )
        };
        let vm = match vm {
            Ok(vm) => vm,
            Err(err) => {
                log::warn!("{what}: no java vm: {err}");
                return false;
            }
        };
        let mut env = match vm.attach_current_thread() {
            Ok(env) => env,
            Err(err) => {
                log::warn!("{what}: {err}");
                return false;
            }
        };
        // the local references made in there are freed at the end
        let res = env.with_local_frame(32, |env| f(env, &activity));
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        if let Err(err) = &res {
            log::warn!("{what}: {err}");
        }
        res.is_ok()
    }

    extern "system" fn native_open(mut env: JNIEnv, _class: JClass, trade: JString) {
        if let (Some(f), Ok(trade)) = (ON_OPEN.get(), env.get_string(&trade)) {
            f(trade.into());
        }
    }

    /// have `f` told of the notifications tapped, through the native method of the activity
    pub fn on_open(f: Box<dyn Fn(String) + Send + Sync>) {
        if ON_OPEN.set(f).is_err() {
            return;
        }
        std::thread::spawn(|| {
            with_activity("failed to listen to notification taps", |env, activity| {
                // the context isn't the activity, and native threads only find system classes
                let loader = env
                    .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
                    .l()?;
                let name = env.new_string(ACTIVITY)?;
                let class: JClass = env
                    .call_method(
                        &loader,
                        "loadClass",
                        "(Ljava/lang/String;)Ljava/lang/Class;",
                        &[(&name).into()],
                    )?
                    .l()?
                    .into();
                env.register_native_methods(
                    &class,
                    &[NativeMethod {
                        name: "nativeOpen".into(),
                        sig: "(Ljava/lang/String;)V".into(),
                        fn_ptr: native_open as *mut _,
                    }],
                )?;
                // the tap that started the app, if it did
                let pending = env
                    .call_static_method(&class, "ready", "()Ljava/lang/String;", &[])?
                    .l()?;
                if !pending.is_null() {
                    let trade: String = env.get_string(&JString::from(pending))?.into();
                    if let Some(f) = ON_OPEN.get() {
                        f(trade);
                    }
                }
                Ok(())
            });
        });
    }

    /// start or stop the foreground service. starting it needs the app to be in the foreground,
    /// which it is when an offer is made or a trade taken, and when it starts with one already.
    pub fn set_background(on: bool) {
        if BACKGROUND.swap(on, Ordering::SeqCst) == on {
            return;
        }
        std::thread::spawn(move || {
            let ok = with_activity(
                "failed to change the foreground service",
                |env, activity| {
                    let intent = env.new_object("android/content/Intent", "()V", &[])?;
                    let class = env.new_string(SERVICE)?;
                    env.call_method(
                        &intent,
                        "setClassName",
                        "(Landroid/content/Context;Ljava/lang/String;)Landroid/content/Intent;",
                        &[activity.into(), (&class).into()],
                    )?;
                    if on {
                        env.call_method(
                            activity,
                            "startForegroundService",
                            "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                            &[(&intent).into()],
                        )?;
                    } else {
                        env.call_method(
                            activity,
                            "stopService",
                            "(Landroid/content/Intent;)Z",
                            &[(&intent).into()],
                        )?;
                    }
                    Ok(())
                },
            );
            if !ok {
                // so the next check tries again
                BACKGROUND.store(!on, Ordering::SeqCst);
            }
        });
    }

    pub fn show(title: String, body: String, tag: String) {
        std::thread::spawn(move || {
            with_activity("failed to show a notification", |env, activity| {
                post(env, activity, &title, &body, &tag)
            })
        });
    }

    fn post(
        env: &mut JNIEnv,
        activity: &JObject,
        title: &str,
        body: &str,
        tag: &str,
    ) -> jni::errors::Result<()> {
        let service = env.new_string("notification")?;
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&service).into()],
            )?
            .l()?;

        // creating a channel that exists already does nothing
        let channel_id = env.new_string(CHANNEL)?;
        let channel_name = env.new_string("trades")?;
        let channel = env.new_object(
            "android/app/NotificationChannel",
            "(Ljava/lang/String;Ljava/lang/CharSequence;I)V",
            &[
                (&channel_id).into(),
                (&channel_name).into(),
                JValue::Int(IMPORTANCE_HIGH),
            ],
        )?;
        env.call_method(
            &manager,
            "createNotificationChannel",
            "(Landroid/app/NotificationChannel;)V",
            &[(&channel).into()],
        )?;

        let builder = env.new_object(
            "android/app/Notification$Builder",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[activity.into(), (&channel_id).into()],
        )?;
        const SELF: &str = "Landroid/app/Notification$Builder;";
        // the app has no monochrome icon of its own, which the status bar needs
        let icon = env
            .get_static_field("android/R$drawable", "stat_notify_chat", "I")?
            .i()?;
        env.call_method(
            &builder,
            "setSmallIcon",
            format!("(I){SELF}"),
            &[JValue::Int(icon)],
        )?;
        let title = env.new_string(title)?;
        env.call_method(
            &builder,
            "setContentTitle",
            format!("(Ljava/lang/CharSequence;){SELF}"),
            &[(&title).into()],
        )?;
        let body = env.new_string(body)?;
        env.call_method(
            &builder,
            "setContentText",
            format!("(Ljava/lang/CharSequence;){SELF}"),
            &[(&body).into()],
        )?;
        env.call_method(
            &builder,
            "setAutoCancel",
            format!("(Z){SELF}"),
            &[JValue::Bool(1)],
        )?;

        // tapping it opens the trade in the running app, or starts it
        let intent = env.new_object("android/content/Intent", "()V", &[])?;
        let class = env.new_string(ACTIVITY)?;
        env.call_method(
            &intent,
            "setClassName",
            "(Landroid/content/Context;Ljava/lang/String;)Landroid/content/Intent;",
            &[activity.into(), (&class).into()],
        )?;
        let tag = env.new_string(tag)?;
        let extra = env.new_string(TRADE)?;
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
            &[(&extra).into(), (&tag).into()],
        )?;
        env.call_method(
            &intent,
            "setFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(INTENT_FLAGS)],
        )?;
        // one pending intent for each trade, or they would all share the last one's extra
        let code = env.call_method(&tag, "hashCode", "()I", &[])?.i()?;
        let pending = env
            .call_static_method(
                "android/app/PendingIntent",
                "getActivity",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[
                    activity.into(),
                    JValue::Int(code),
                    (&intent).into(),
                    JValue::Int(PENDING_INTENT_FLAGS),
                ],
            )?
            .l()?;
        env.call_method(
            &builder,
            "setContentIntent",
            format!("(Landroid/app/PendingIntent;){SELF}"),
            &[(&pending).into()],
        )?;

        let notification = env
            .call_method(&builder, "build", "()Landroid/app/Notification;", &[])?
            .l()?;
        env.call_method(
            &manager,
            "notify",
            "(Ljava/lang/String;ILandroid/app/Notification;)V",
            &[(&tag).into(), JValue::Int(0), (&notification).into()],
        )?;
        Ok(())
    }

    /// android 13 and later only show them once the user allowed it
    pub fn request_permission() {
        std::thread::spawn(|| {
            with_activity(
                "failed to ask for the notification permission",
                |env, activity| {
                    let sdk = env
                        .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
                        .i()?;
                    if sdk < 33 {
                        return Ok(());
                    }
                    let permission = env.new_string("android.permission.POST_NOTIFICATIONS")?;
                    let granted = env
                        .call_method(
                            activity,
                            "checkSelfPermission",
                            "(Ljava/lang/String;)I",
                            &[(&permission).into()],
                        )?
                        .i()?;
                    // PackageManager.PERMISSION_GRANTED
                    if granted == 0 {
                        return Ok(());
                    }
                    let permissions = env.new_object_array(1, "java/lang/String", &permission)?;
                    env.call_method(
                        activity,
                        "requestPermissions",
                        "([Ljava/lang/String;I)V",
                        &[(&permissions).into(), JValue::Int(1)],
                    )?;
                    Ok(())
                },
            )
        });
    }
}
