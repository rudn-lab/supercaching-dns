use std::time::Duration;

use hickory_client::rr::{Record, RecordData};
use hickory_resolver::name_server::{GenericConnector, TokioRuntimeProvider};
use hickory_server::{
    authority::MessageResponseBuilder,
    proto::op::Header,
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    ServerFuture,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let handler = Handler {
        resolver: hickory_resolver::AsyncResolver::tokio_from_system_conf()
            .expect("Failed to create resolver from system configuration"),
    };
    let mut srv = ServerFuture::new(handler);
    srv.register_listener(
        tokio::net::TcpListener::bind("127.0.0.1:5356")
            .await
            .expect("Failed to bind to port tcp/5356"),
        Duration::from_secs(10),
    );
    srv.register_socket(
        tokio::net::UdpSocket::bind("127.0.0.1:5356")
            .await
            .expect("Failed to bind to port udp/5356"),
    );
    srv.block_until_done().await.expect("Server shut down");
}
struct Handler {
    pub resolver: hickory_resolver::AsyncResolver<GenericConnector<TokioRuntimeProvider>>,
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
                let resp = MessageResponseBuilder::from_message_request(&request);
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

                        let mut header = Header::new();
                        header.set_id(request.id());
                        let mut answers = vec![];
                        let mut name_servers = vec![];
                        let mut soa = vec![];
                        let mut additionals = vec![];
                        for record in data.record_iter() {
                            if record.record_type().is_soa() {
                                soa.push(record);
                                continue;
                            }
                            if record.record_type().is_ns() {
                                name_servers.push(record);
                                continue;
                            }

                            // If the record refers to the query name, it's an answer
                            if record.name().to_string() == query.name().to_string() {
                                answers.push(record);
                            } else {
                                additionals.push(record);
                            }
                        }

                        header.set_answer_count(answers.len() as u16);
                        header.set_name_server_count(name_servers.len() as u16);
                        header.set_additional_count(additionals.len() as u16);
                        header.set_message_type(hickory_client::op::MessageType::Response);
                        let msg = MessageResponseBuilder::from_message_request(&request).build(
                            header,
                            answers,
                            name_servers,
                            soa,
                            additionals,
                        );

                        // TODO: implement supercaching
                        return response_handle
                            .send_response(msg)
                            .await
                            .expect("Failed to send response to successful upstream request");
                    }
                    Err(why) => {
                        let mut attempt_supercaching = true;
                        match why.kind() {
                            hickory_resolver::error::ResolveErrorKind::NoConnections => {
                                tracing::error!("The upstream DNS resolver is not configured with any servers -- this is a config error!");
                                attempt_supercaching = false;
                                let resp = MessageResponseBuilder::from_message_request(&request);
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
                                attempt_supercaching = false;
                                let resp = MessageResponseBuilder::from_message_request(&request);
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
                                header.set_response_code(response_code.clone());
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
                        tracing::error!("Failed to query upstream DNS server: {}", why);
                        // TODO: implement supercaching
                        if attempt_supercaching {
                            // TODO: implement supercaching
                            todo!();
                        } else {
                            let resp = MessageResponseBuilder::from_message_request(&request);
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
