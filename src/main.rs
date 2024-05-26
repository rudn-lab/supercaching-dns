mod args;

use std::time::Duration;

use hickory_client::rr::{Record, RecordData};
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    name_server::{GenericConnector, TokioRuntimeProvider},
};
use hickory_server::{
    authority::{MessageResponse, MessageResponseBuilder},
    proto::op::Header,
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    ServerFuture,
};

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    use clap::Parser;
    let args = args::Args::parse();

    let db = sqlx::SqlitePool::connect(
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL not set")
            .as_str(),
    )
    .await
    .expect("Failed to connect to database");
    sqlx::migrate!()
        .run(&db)
        .await
        .expect("Failed to run migrations");

    let mut config = ResolverConfig::new();
    for server in args.upstream_servers {
        config.add_name_server(server.to_name_server_config());
    }
    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_secs(args.upstream_timeout);

    let handler = Handler {
        resolver: hickory_resolver::AsyncResolver::tokio(config, opts),
        db,
    };
    let mut srv = ServerFuture::new(handler);

    let listener_addr = std::net::SocketAddr::new(args.bind_address, args.bind_port);

    srv.register_listener(
        tokio::net::TcpListener::bind(listener_addr)
            .await
            .expect("Failed to bind to TCP port"),
        Duration::from_secs(10),
    );
    srv.register_socket(
        tokio::net::UdpSocket::bind(listener_addr)
            .await
            .expect("Failed to bind to UDP port"),
    );
    srv.block_until_done().await.expect("Server shut down");
}
struct Handler {
    pub resolver: hickory_resolver::AsyncResolver<GenericConnector<TokioRuntimeProvider>>,
    pub db: sqlx::SqlitePool,
}

#[derive(Debug, Clone, Default)]
struct RecordsByKind {
    pub answers: Vec<Record>,
    pub name_servers: Vec<Record>,
    pub soa: Vec<Record>,
    pub additionals: Vec<Record>,
}

fn sort_out_records(
    request: &Request,
    records: &[Record],
    received_at: std::time::SystemTime,
) -> RecordsByKind {
    let mut d = RecordsByKind::default();
    for record in records {
        // Update the TTL of the record
        // The TTL in the record is correct for the time of the query,
        // so we need to update it to be relative to the current time
        let mut record = record.clone();
        let ttl = record.ttl();
        let ttl_expires_at = received_at + std::time::Duration::from_secs(ttl as u64);
        let time_until_expiration = ttl_expires_at
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default();
        record.set_ttl(time_until_expiration.as_secs() as u32);

        if record.record_type().is_soa() {
            d.soa.push(record);
            continue;
        }
        if record.record_type().is_ns() {
            d.name_servers.push(record);
            continue;
        }

        // If the record refers to the query name, it's an answer
        if record.name().to_string() == request.query().name().to_string() {
            d.answers.push(record);
        } else {
            d.additionals.push(record);
        }
    }

    d
}

impl RecordsByKind {
    pub fn make_response<'a>(
        &'a self,
        request: &'a Request,
    ) -> MessageResponse<
        'a,
        'a,
        std::slice::Iter<Record>,
        std::slice::Iter<Record>,
        std::slice::Iter<Record>,
        std::slice::Iter<Record>,
    > {
        let mut header = Header::new();
        header.set_id(request.id());

        header.set_answer_count(self.answers.len() as u16);
        header.set_name_server_count(self.name_servers.len() as u16);
        header.set_additional_count(self.additionals.len() as u16);
        header.set_message_type(hickory_client::op::MessageType::Response);
        let msg = MessageResponseBuilder::from_message_request(request).build(
            header,
            self.answers.iter(),
            self.name_servers.iter(),
            self.soa.iter(),
            self.additionals.iter(),
        );
        msg
    }
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        match request.message_type() {
            hickory_server::proto::op::MessageType::Response => {
                tracing::warn!("Received a response message from client: {request:?}");
                // This is a server, so we don't expect any responses to come our way
                let resp = MessageResponseBuilder::from_message_request(request);
                let msg = resp.error_msg(
                    &Header::new(),
                    hickory_server::proto::op::ResponseCode::Refused,
                );
                return response_handle
                    .send_response(msg)
                    .await
                    .expect("Failed to send response to unexpected response");
            }
            hickory_server::proto::op::MessageType::Query => {
                tracing::debug!("Received query from client: {request:?}");
                // Try sending this query to the upstream DNS server
                let query = request.query();
                let resp = self.resolver.lookup(query.name(), query.query_type()).await;
                match resp {
                    Ok(data) => {
                        tracing::debug!("Received response from upstream: {data:?}");

                        // Record the response in the database for caching
                        tokio::spawn({
                            let db = self.db.clone();
                            let query = query.clone();
                            let data = data.clone();

                            let record_name = query.name().to_string();
                            let record_type = query.query_type().to_string();
                            let record_data =
                                serde_json::to_string(&data.record_iter().collect::<Vec<_>>())
                                    .expect("failed to serialize record data");

                            let last_query_at_unix = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                                as i64;
                            let data_received_at_unix = last_query_at_unix;

                            async move {
                                sqlx::query!("INSERT INTO record (record_name, record_type, content_json, data_received_at_unix, last_query_at_unix) VALUES (?,?,?,?,?) ON CONFLICT (record_name, record_type) DO UPDATE SET content_json = ?, data_received_at_unix = ?, last_query_at_unix = ?",
                                    record_name,
                                    record_type,
                                    record_data,
                                    data_received_at_unix,
                                    last_query_at_unix,
                                    record_data,
                                    data_received_at_unix,
                                    last_query_at_unix,
                                ).execute(&db).await.expect("Failed to insert record")
                            }
                        });

                        let records = data.records();
                        let received_at = std::time::SystemTime::now();
                        let msg = sort_out_records(request, records, received_at);
                        let msg = msg.make_response(request);

                        return response_handle
                            .send_response(msg)
                            .await
                            .expect("Failed to send response to successful upstream request");
                    }
                    Err(why) => {
                        match why.kind() {
                            hickory_resolver::error::ResolveErrorKind::NoConnections => {
                                tracing::error!("The upstream DNS resolver is not configured with any servers -- this is a config error!");
                                let resp = MessageResponseBuilder::from_message_request(request);
                                let mut header = Header::new();
                                header.set_id(request.id());
                                let msg = resp.error_msg(
                                    &header,
                                    hickory_server::proto::op::ResponseCode::ServFail,
                                );
                                return response_handle
                                    .send_response(msg)
                                    .await
                                    .expect("Failed to send response to failed upstream request");
                            }
                            hickory_resolver::error::ResolveErrorKind::NoRecordsFound {
                                soa,
                                response_code,
                                ..
                            } => {
                                // In this case, the upstream has explicitly told us that there is no such domain
                                // We won't cache this at all, apart from whatever caching is in hickory and the OS.
                                let resp = MessageResponseBuilder::from_message_request(request);
                                let mut header = Header::new();
                                let mut my_soa = vec![];
                                if let Some(record) = soa {
                                    let record = *record.clone();
                                    let data = record.data().unwrap();
                                    let mut new_record = Record::with(
                                        record.name().clone(),
                                        record.record_type(),
                                        record.ttl(),
                                    );
                                    new_record.set_data(Some(data.clone().into_rdata()));
                                    my_soa.push(new_record);
                                }

                                header.set_id(request.id());
                                header.set_response_code(*response_code);
                                let msg = resp.build(header, vec![], vec![], my_soa.iter(), vec![]);
                                return response_handle.send_response(msg).await.expect(
                                    "Failed to send negative response to upstream request",
                                );
                            }
                            why => {
                                // This is where generic errors, like protocol errors or timeouts, end up
                                tracing::error!("Failed to resolve query: {why:?}");
                            }
                        };
                        tracing::warn!("Failed to query upstream DNS server: {}", why);

                        // Try fetching the response from the cache
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64;
                        let record_name = query.name().to_string();
                        let record_type = query.query_type().to_string();
                        let item = sqlx::query!("UPDATE record SET last_query_at_unix = ? WHERE record_name = ? AND record_type = ? RETURNING content_json, data_received_at_unix",
                            now,
                            record_name,
                            record_type
                        ).fetch_optional(&self.db).await.expect("Failed to fetch record");

                        if let Some(data) = item {
                            let records: Vec<Record> = serde_json::from_str(&data.content_json)
                                .expect("Failed to parse record content in database");

                            let received_at = std::time::SystemTime::UNIX_EPOCH
                                + std::time::Duration::from_secs(data.data_received_at_unix as u64);

                            let msg = sort_out_records(request, &records, received_at);
                            let msg = msg.make_response(request);

                            return response_handle
                                .send_response(msg)
                                .await
                                .expect("Failed to send response to failed upstream request");
                        } else {
                            tracing::warn!("Failed to find record in database in reference to failed upstream lookup: {record_name} {record_type}");
                            let resp = MessageResponseBuilder::from_message_request(request);
                            let mut header = Header::new();
                            header.set_id(request.id());
                            let msg = resp.error_msg(
                                &header,
                                hickory_server::proto::op::ResponseCode::ServFail,
                            );
                            return response_handle
                                .send_response(msg)
                                .await
                                .expect("Failed to send response to failed upstream request");
                        }
                    }
                }
            }
        }
    }
}
