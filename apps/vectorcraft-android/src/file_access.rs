//! "All files access" (MANAGE_EXTERNAL_STORAGE), which the in-app file browser needs to browse
//! shared storage by path. It is a special app access, not a runtime permission: the app can't
//! ask for it with a dialog, only open the Settings screen where the user turns it on. Both calls
//! go through JNI to the activity that started the app.

use jni::JavaVM;
use jni::objects::{JObject, JString, JValue};

/// Run `f` with the JNI environment and the activity. A Java exception is cleared (never left
/// pending) and returned as the error, with its message.
fn with_activity<R>(what: &str, f: impl FnOnce(&mut jni::JNIEnv, &JObject) -> jni::errors::Result<R>) -> Result<R, String> {
    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err(format!("{what}: Android's JNI context isn't set"));
    }
    #[allow(unsafe_code)]
    // SAFETY: android-activity fills ndk-context with the process's JavaVM and the activity
    // before `android_main` runs, and both live as long as the process (checked non-null above).
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("{what}: no JavaVM ({e})"))?;
    let mut env = vm.attach_current_thread_permanently().map_err(|e| format!("{what}: can't attach to Java ({e})"))?;
    #[allow(unsafe_code)]
    // SAFETY: a global reference to the activity, owned by android-activity; wrapping it doesn't
    // take ownership (JObject never deletes the reference).
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result = f(&mut env, &activity);
    let exception = java_exception(&mut env);
    result.map_err(|e| {
        let m = match exception {
            Some(java) => format!("{what}: {java}"),
            None => format!("{what}: {e}"),
        };
        log::warn!("{m}");
        m
    })
}

/// Take the pending Java exception, if any, as its `toString()`.
fn java_exception(env: &mut jni::JNIEnv) -> Option<String> {
    if !env.exception_check().unwrap_or(false) {
        return None;
    }
    let thrown = env.exception_occurred().ok();
    let _ = env.exception_clear();
    let thrown = thrown?;
    let text = env.call_method(&thrown, "toString", "()Ljava/lang/String;", &[]).and_then(|v| v.l()).ok()?;
    let _ = env.exception_clear();
    env.get_string(&JString::from(text)).ok().map(Into::into)
}

/// Whether the user has given PdfCraft All files access.
pub fn granted() -> bool {
    match with_activity("All files access", |env, _| env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z()) {
        Ok(granted) => granted,
        Err(_) => false,
    }
}

/// `Intent.FLAG_ACTIVITY_NEW_TASK`: Settings opens as its own task, on top of PdfCraft.
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// Open Settings on PdfCraft's All files access switch, or else on the list of apps with that
/// access. The error says why neither opened.
pub fn request() -> Result<(), String> {
    log::info!("opening All files access settings");
    let per_app = with_activity("opening PdfCraft's All files access switch", |env, activity| {
        let package = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        let package: String = env.get_string(&JString::from(package))?.into();
        let uri_text = env.new_string(format!("package:{package}"))?;
        let uri = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri_text)])?.l()?;
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let intent =
            env.new_object("android/content/Intent", "(Ljava/lang/String;Landroid/net/Uri;)V", &[JValue::Object(&action), JValue::Object(&uri)])?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)])?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    });
    let Err(first) = per_app else { return Ok(()) };
    // Some devices lack the per-app screen: the list of all apps instead.
    with_activity("opening the All files access list", |env, activity| {
        let action = env.new_string("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION")?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)])?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    })
    .map_err(|second| format!("{first}; {second}"))
}
