mod agent_context;
mod app;
mod args;
mod backend;
mod config;
mod credentials;
mod daemon;
mod focused_window;
mod http_client;
mod llm;
mod output;
mod paths;
mod settings_backend;
mod setup;
mod state;
mod wav;
mod waveform;

fn main() {
    init_tls_crypto_provider();
    if let Err(error) = app::run() {
        eprintln!("voice-input: {error:#}");
        std::process::exit(1);
    }
}

fn init_tls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
