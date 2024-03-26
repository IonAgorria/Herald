use actix_http::header;
use actix_web::guard::{Guard, GuardContext};

pub fn header_insensitive(name: header::HeaderName, value: &'static str) -> impl Guard {
    let value_low = value.to_lowercase();
    //Check if valid
    header::HeaderValue::from_str(value_low.as_str()).unwrap();
    HeaderGuardInsensitive(
        name,
        value_low,
    )
}

struct HeaderGuardInsensitive(header::HeaderName, String);

impl Guard for HeaderGuardInsensitive {
    fn check(&self, ctx: &GuardContext<'_>) -> bool {
        if let Some(val) = ctx.head().headers.get(&self.0) {
            if let Ok(val_str) = val.to_str() {
                return val_str.to_lowercase() == self.1;
            }
        }

        false
    }
}
