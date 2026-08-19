mod cucumber_support;
mod support;

mod auth {
    pub use lg_buddy::auth::SystemUser;
}

mod platform_access_token {
    pub use lg_buddy::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
}

#[path = "cucumber_support/webos.rs"]
mod web_os;

use cucumber::World as _;
use cucumber_support::world::LgBuddyWorld;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    LgBuddyWorld::cucumber()
        .max_concurrent_scenarios(1)
        .run_and_exit(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/features"))
        .await;
}
