//! QUIC server for the Orchestra plugin protocol.
//!
//! Listens for incoming QUIC connections from the orchestrator,
//! accepts bidirectional streams, reads PluginRequest messages,
//! dispatches them to the handler, and writes PluginResponse messages.
//!
//! TLS certificate handling mirrors the Go SDK (`plugin.ServerTLSConfig`):
//! when `--certs-dir` is provided, the server loads the shared CA from
//! `ca.crt`/`ca.key` and generates a CA-signed server certificate named
//! `engine.rag.crt`/`engine.rag.key`. This ensures the orchestrator
//! (which trusts the same CA) can verify the plugin's identity.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::framing::{read_message, write_message};
use super::handler::RequestHandler;
use crate::proto::orchestra::plugin::v1::PluginRequest;

/// Plugin name used for certificate generation (matches the plugin ID).
const PLUGIN_NAME: &str = "engine.rag";

/// QUIC-based plugin server.
///
/// Accepts connections from the orchestrator, handles PluginRequest/PluginResponse
/// exchanges over bidirectional QUIC streams.
pub struct PluginServer {
    handler: Arc<RequestHandler>,
    listen_addr: SocketAddr,
    certs_dir: Option<PathBuf>,
}

impl PluginServer {
    pub fn new(
        handler: RequestHandler,
        listen_addr: SocketAddr,
        certs_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            handler: Arc::new(handler),
            listen_addr,
            certs_dir,
        }
    }

    /// Start the QUIC server and listen for connections until cancellation.
    ///
    /// Returns the actual bound address (useful when listen_addr port is 0).
    pub async fn listen_and_serve(&self, cancel: CancellationToken) -> Result<SocketAddr> {
        let server_config = self.build_server_config()?;
        let endpoint = Endpoint::server(server_config, self.listen_addr)
            .context("failed to create QUIC endpoint")?;

        let local_addr = endpoint.local_addr()?;
        info!(addr = %local_addr, "QUIC server listening");

        // Print READY to stderr so the orchestrator knows we are alive
        eprintln!("READY {local_addr}");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("shutdown signal received, closing QUIC endpoint");
                    endpoint.close(0u32.into(), b"shutdown");
                    break;
                }
                incoming = endpoint.accept() => {
                    match incoming {
                        Some(incoming_conn) => {
                            let handler = Arc::clone(&self.handler);
                            let cancel = cancel.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_connection(incoming_conn, handler, cancel).await {
                                    warn!(error = %e, "connection handler failed");
                                }
                            });
                        }
                        None => {
                            info!("QUIC endpoint closed");
                            break;
                        }
                    }
                }
            }
        }

        Ok(local_addr)
    }

    /// Handle a single QUIC connection: accept bidirectional streams in a loop.
    async fn handle_connection(
        incoming: quinn::Incoming,
        handler: Arc<RequestHandler>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let connection = incoming.await.context("failed to accept QUIC connection")?;
        let remote = connection.remote_address();
        info!(remote = %remote, "accepted QUIC connection");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!(remote = %remote, "connection handler cancelled");
                    break;
                }
                stream = connection.accept_bi() => {
                    match stream {
                        Ok((send, recv)) => {
                            let handler = Arc::clone(&handler);
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_stream(send, recv, handler).await {
                                    debug!(error = %e, "stream handler completed with error");
                                }
                            });
                        }
                        Err(quinn::ConnectionError::ApplicationClosed(_)) => {
                            info!(remote = %remote, "connection closed by peer");
                            break;
                        }
                        Err(e) => {
                            warn!(remote = %remote, error = %e, "failed to accept stream");
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle a single bidirectional QUIC stream:
    /// read PluginRequest -> dispatch -> write PluginResponse -> finish.
    async fn handle_stream(
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        handler: Arc<RequestHandler>,
    ) -> Result<()> {
        let request: PluginRequest = read_message(&mut recv).await
            .context("failed to read PluginRequest")?;

        debug!(request_id = %request.request_id, "received request");

        let response = handler.handle_request(request).await;

        write_message(&mut send, &response).await
            .context("failed to write PluginResponse")?;

        send.finish()?;

        Ok(())
    }

    /// Build a quinn::ServerConfig with TLS certificates.
    ///
    /// When certs_dir is provided (normal mode), loads the shared CA and
    /// generates a CA-signed server cert — matching the Go SDK's ServerTLSConfig.
    /// When no certs_dir is provided (test mode), generates self-signed certs.
    fn build_server_config(&self) -> Result<quinn::ServerConfig> {
        let (cert_chain, key) = if let Some(ref certs_dir) = self.certs_dir {
            Self::load_or_generate_ca_signed_cert(certs_dir)?
        } else {
            Self::generate_self_signed_certs()?
        };

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .context("failed to configure TLS with certificates")?;

        server_crypto.alpn_protocols = vec![b"orchestra-plugin".to_vec()];

        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .context("failed to create QUIC server config from TLS config")?,
        ));

        Ok(server_config)
    }

    /// Load the shared CA from certs_dir and generate a CA-signed server cert.
    ///
    /// This mirrors the Go SDK's `GenerateCert()` + `ServerTLSConfig()`:
    /// 1. Load ca.crt + ca.key from certs_dir
    /// 2. If engine.rag.crt/key exists, load it
    /// 3. Otherwise generate a new cert signed by the CA with SANs for localhost
    fn load_or_generate_ca_signed_cert(
        certs_dir: &PathBuf,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let cert_path = certs_dir.join(format!("{PLUGIN_NAME}.crt"));
        let key_path = certs_dir.join(format!("{PLUGIN_NAME}.key"));

        // Try loading existing cert first
        if cert_path.exists() && key_path.exists() {
            info!(dir = %certs_dir.display(), "loading existing CA-signed certificate");
            return Self::load_pem_cert_and_key(&cert_path, &key_path);
        }

        // Load the shared CA
        let ca_cert_path = certs_dir.join("ca.crt");
        let ca_key_path = certs_dir.join("ca.key");

        if !ca_cert_path.exists() || !ca_key_path.exists() {
            anyhow::bail!(
                "shared CA not found at {}/ca.crt — the orchestrator must start first to generate the CA",
                certs_dir.display()
            );
        }

        info!(dir = %certs_dir.display(), "generating CA-signed certificate for {}", PLUGIN_NAME);

        // Parse CA cert
        let ca_cert_pem = std::fs::read(&ca_cert_path)
            .context("failed to read ca.crt")?;
        let ca_cert_der: CertificateDer<'static> = rustls_pemfile::certs(&mut &ca_cert_pem[..])
            .next()
            .ok_or_else(|| anyhow::anyhow!("no certificate found in ca.crt"))?
            .context("failed to parse ca.crt PEM")?;

        let ca_params = rcgen::CertificateParams::from_ca_cert_der(&ca_cert_der)
            .context("failed to parse CA certificate DER")?;

        // Parse CA key
        let ca_key_pem = std::fs::read(&ca_key_path)
            .context("failed to read ca.key")?;
        let ca_key_der = rustls_pemfile::private_key(&mut &ca_key_pem[..])
            .context("failed to parse ca.key PEM")?
            .context("no private key found in ca.key")?;

        let ca_key_pair = rcgen::KeyPair::try_from(ca_key_der.secret_der())
            .context("failed to load CA key pair")?;

        let ca_cert = ca_params.self_signed(&ca_key_pair)
            .context("failed to reconstruct CA certificate")?;

        // Generate server cert signed by CA
        let mut server_params = rcgen::CertificateParams::new(vec![
            PLUGIN_NAME.to_string(),
            "localhost".to_string(),
        ])
        .context("failed to create server certificate params")?;

        server_params.distinguished_name.push(
            rcgen::DnType::OrganizationName,
            "Orchestra",
        );
        server_params.distinguished_name.push(
            rcgen::DnType::CommonName,
            PLUGIN_NAME,
        );

        // Add IP SANs for localhost (matching Go SDK)
        server_params.subject_alt_names.push(
            rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        );
        server_params.subject_alt_names.push(
            rcgen::SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
        );

        let server_key_pair = rcgen::KeyPair::generate()
            .context("failed to generate server key pair")?;

        let server_cert = server_params
            .signed_by(&server_key_pair, &ca_cert, &ca_key_pair)
            .context("failed to sign server certificate with CA")?;

        let cert_der = CertificateDer::from(server_cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            server_key_pair.serialize_der(),
        ));

        // Persist for reuse across restarts
        let cert_pem = pem::encode(&pem::Pem::new(
            "CERTIFICATE",
            cert_der.as_ref().to_vec(),
        ));
        std::fs::write(&cert_path, cert_pem.as_bytes())
            .context("failed to write server certificate")?;

        let key_pem = pem::encode(&pem::Pem::new(
            "PRIVATE KEY",
            server_key_pair.serialize_der(),
        ));
        std::fs::write(&key_path, key_pem.as_bytes())
            .context("failed to write server private key")?;

        info!(cert = %cert_path.display(), "CA-signed certificate generated and saved");

        Ok((vec![cert_der], key_der))
    }

    /// Load PEM-encoded certificate and private key from disk.
    fn load_pem_cert_and_key(
        cert_path: &PathBuf,
        key_path: &PathBuf,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let cert_pem = std::fs::read(cert_path)
            .context("failed to read certificate")?;
        let key_pem = std::fs::read(key_path)
            .context("failed to read private key")?;

        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut &cert_pem[..])
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to parse certificate PEM")?;

        let key = rustls_pemfile::private_key(&mut &key_pem[..])
            .context("failed to parse private key PEM")?
            .context("no private key found in PEM file")?;

        Ok((certs, key))
    }

    /// Generate self-signed certs (used when no certs_dir is provided, e.g. tests).
    fn generate_self_signed_certs(
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .context("failed to create certificate params")?;
        params.distinguished_name.push(
            rcgen::DnType::OrganizationName,
            "Orchestra Plugin",
        );
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            "orchestra-rag",
        );

        let key_pair = rcgen::KeyPair::generate()
            .context("failed to generate key pair")?;

        let cert = params
            .self_signed(&key_pair)
            .context("failed to generate self-signed certificate")?;

        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            key_pair.serialize_der(),
        ));

        Ok((vec![cert_der], key_der))
    }
}
