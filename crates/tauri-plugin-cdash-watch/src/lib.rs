//! The Android side of notifications: a foreground service that holds the
//! agent's `/api/notify` stream open and posts each note, and the tap that
//! opens its chat. Rust only starts, stops and asks it; nothing is exposed to
//! JavaScript. Elsewhere the plugin registers nothing.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::Runtime;

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("cdash-watch")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin("dev.cdash.watch", "WatchPlugin")?;
                _app.manage(Watch(handle));
            }
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "android")]
mod android {
    use serde_json::{json, Value};
    use tauri::plugin::PluginHandle;
    use tauri::{Manager, Runtime};

    pub struct Watch<R: Runtime>(pub(crate) PluginHandle<R>);

    impl<R: Runtime> Watch<R> {
        fn run(&self, command: &str, args: Value) -> Result<Value, String> {
            self.0.run_mobile_plugin(command, args).map_err(|e| e.to_string())
        }

        /// Asks for what the service needs (permission, battery exemption), then starts it.
        pub fn start(&self, base: &str) -> Result<(), String> {
            self.run("start", json!({ "base": base })).map(drop)
        }

        pub fn stop(&self) -> Result<(), String> {
            self.run("stop", json!({})).map(drop)
        }

        /// Removes the session's notification.
        pub fn clear(&self, session: &str) -> Result<(), String> {
            self.run("clear", json!({ "session": session })).map(drop)
        }

        /// The `{s, p}` of a tap that launched the app, once.
        pub fn take_open(&self) -> Result<Option<Value>, String> {
            let v = self.run("takeOpen", json!({}))?;
            Ok(v.get("s").is_some().then_some(v))
        }
    }

    pub trait WatchExt<R: Runtime> {
        fn watch(&self) -> &Watch<R>;
    }

    impl<R: Runtime, T: Manager<R>> WatchExt<R> for T {
        fn watch(&self) -> &Watch<R> {
            self.state::<Watch<R>>().inner()
        }
    }
}
