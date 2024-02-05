use std::str::FromStr;

pub mod xprm;
pub mod time;

pub fn config_from_str<T: FromStr>(key: &str, default: T) -> T {
    if let Ok(val) = dotenvy::var(key) {
        T::from_str(val.as_str()).unwrap_or(default)
    } else {
        default
    }
}

pub fn config_string(key: &str, default: &str) -> String {
    if let Ok(val) = dotenvy::var(key) {
        val
    } else {
        default.to_string()
    }
}
