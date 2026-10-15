//! system notifications: the desktop's notification service, android's notification manager
//! (through jni), or the browser's notifications. each one is best effort, a failure is only
//! logged.

/// show a notification. a later one with the same `tag` replaces it, where the system allows.
pub fn show(title: &str, body: &str, tag: &str) {
    imp::show(title.to_string(), body.to_string(), tag.to_string());
}

/// ask for the permission to show them, where one is needed and wasn't asked for yet. best called
/// right after the user did something, as browsers ignore it otherwise.
pub fn request_permission() {
    imp::request_permission();
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
mod imp {
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
    use jni::objects::{JObject, JValue};
    use jni::{JNIEnv, JavaVM};

    const CHANNEL: &str = "trades";
    /// NotificationManager.IMPORTANCE_HIGH, so it pops up
    const IMPORTANCE_HIGH: i32 = 4;
    /// PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT
    const PENDING_INTENT_FLAGS: i32 = 0x0400_0000 | 0x0800_0000;

    /// run `f` with the jni env of this thread and the activity
    fn with_activity(
        what: &str,
        f: impl for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> jni::errors::Result<()>,
    ) {
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
            Err(err) => return log::warn!("{what}: no java vm: {err}"),
        };
        let mut env = match vm.attach_current_thread() {
            Ok(env) => env,
            Err(err) => return log::warn!("{what}: {err}"),
        };
        // the local references made in there are freed at the end
        let res = env.with_local_frame(32, |env| f(env, &activity));
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        if let Err(err) = res {
            log::warn!("{what}: {err}");
        }
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

        // tapping it brings the app back, like its launcher icon does
        let pm = env
            .call_method(
                activity,
                "getPackageManager",
                "()Landroid/content/pm/PackageManager;",
                &[],
            )?
            .l()?;
        let package = env
            .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let intent = env
            .call_method(
                &pm,
                "getLaunchIntentForPackage",
                "(Ljava/lang/String;)Landroid/content/Intent;",
                &[(&package).into()],
            )?
            .l()?;
        if !intent.is_null() {
            let pending = env
                .call_static_method(
                    "android/app/PendingIntent",
                    "getActivity",
                    "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                    &[
                        activity.into(),
                        JValue::Int(0),
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
        }

        let notification = env
            .call_method(&builder, "build", "()Landroid/app/Notification;", &[])?
            .l()?;
        let tag = env.new_string(tag)?;
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
            with_activity("failed to ask for the notification permission", |env, activity| {
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
            })
        });
    }
}
