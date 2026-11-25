#![cfg_attr(
    all(windows, not(debug_assertions), feature = "desktop"),
    windows_subsystem = "windows"
)]

mod api;
mod connections;
#[cfg(feature = "desktop")]
mod desktop;
mod gateway;
mod provider;
mod store;
mod types;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--headless") || !cfg!(feature = "desktop") {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(async {
            let server = api::Server::start().await?;
            println!("Zephyr listening on http://127.0.0.1:{}", server.port);
            tokio::signal::ctrl_c().await?;
            server.stop().await;
            Ok(())
        });
    }
    #[cfg(feature = "desktop")]
    desktop::run()?;
    Ok(())
}
