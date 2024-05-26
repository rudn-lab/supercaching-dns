use std::{net::IpAddr, str::FromStr};

use hickory_resolver::config::NameServerConfig;

#[derive(clap::Parser, Clone)]
pub struct Args {
    /// Address to bind my listener to
    #[clap(long, default_value = "127.0.0.1")]
    pub bind_address: IpAddr,

    /// Port to bind my listener to. This includes the UDP and TCP ports
    #[clap(long, default_value = "53")]
    pub bind_port: u16,

    /// How long to wait for an answer from an upstream server before falling back to supercaching.
    /// Keep in mind that clients' timeout is usually around 5 seconds,
    /// so this needs to be smaller.
    #[clap(long, default_value = "3")]
    pub upstream_timeout: u64,

    /// List of upstream DNS servers to try, separated by commas, in the format "address[:port][/protocol]".
    /// By default, the port is 53 and the protocol is udp
    #[clap(short, long, value_delimiter = ',', num_args = 1.., required = true)]
    pub upstream_servers: Vec<UpstreamSpec>,
}

#[derive(Clone)]
pub struct UpstreamSpec {
    pub host: IpAddr,
    pub port: u16,
    pub protocol: hickory_resolver::config::Protocol,
}

impl UpstreamSpec {
    pub fn to_name_server_config(&self) -> NameServerConfig {
        NameServerConfig::new(
            std::net::SocketAddr::new(self.host, self.port),
            self.protocol,
        )
    }
}

impl FromStr for UpstreamSpec {
    type Err = String;

    fn from_str(mut s: &str) -> Result<Self, Self::Err> {
        // If there is a colon in the string, then there is a port part
        // If there is a slash in the string, then there is a protocol part

        // First extract the part before the colon and slash
        let mut first_separator = s.len();
        if let Some(idx) = s.find(':') {
            first_separator = idx;
        }
        if let Some(idx) = s.find('/') {
            first_separator = first_separator.min(idx);
        }

        let host = s[..first_separator].to_string();
        if host.is_empty() {
            return Err("No host specified".to_string());
        }
        let host: IpAddr = host
            .parse()
            .map_err(|_| format!("Failed to parse IP: {host}"))?;

        let mut port = 53;
        let mut protocol = hickory_resolver::config::Protocol::Udp;

        if first_separator == s.len() {
            // No port or protocol specified
            return Ok(UpstreamSpec {
                host,
                port,
                protocol,
            });
        }

        if let Some(idx) = s.find('/') {
            let proto = &s[idx + 1..];
            match proto {
                "tcp" => protocol = hickory_resolver::config::Protocol::Tcp,
                "udp" => protocol = hickory_resolver::config::Protocol::Udp,
                _ => return Err(format!("Unknown protocol: {proto}")),
            }

            // Now that we've extracted the protocol, remove it from the string
            s = &s[..idx];
        }

        if let Some(idx) = s.find(':') {
            let port_str = &s[idx + 1..];
            port = port_str
                .parse::<u16>()
                .map_err(|_| format!("Failed to parse port: {port_str}"))?;
        }

        Ok(UpstreamSpec {
            host,
            port,
            protocol,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_spec() {
        let spec: UpstreamSpec = "127.0.0.1".parse().unwrap();
        assert_eq!(spec.host, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(spec.port, 53);
        assert_eq!(spec.protocol, hickory_resolver::config::Protocol::Udp);

        let spec: UpstreamSpec = "127.0.0.1:80".parse().unwrap();
        assert_eq!(spec.host, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(spec.port, 80);
        assert_eq!(spec.protocol, hickory_resolver::config::Protocol::Udp);

        let spec: UpstreamSpec = "127.0.0.1:80/udp".parse().unwrap();
        assert_eq!(spec.host, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(spec.port, 80);
        assert_eq!(spec.protocol, hickory_resolver::config::Protocol::Udp);

        let spec: UpstreamSpec = "127.0.0.1/tcp".parse().unwrap();
        assert_eq!(spec.host, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(spec.port, 53);
        assert_eq!(spec.protocol, hickory_resolver::config::Protocol::Tcp);

        let spec: UpstreamSpec = "127.0.0.1:80/tcp".parse().unwrap();
        assert_eq!(spec.host, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(spec.port, 80);
        assert_eq!(spec.protocol, hickory_resolver::config::Protocol::Tcp);
    }

    #[test]
    fn test_upstream_spec_bad() {
        assert!("".parse::<UpstreamSpec>().is_err());
        assert!("127.0.0.1/tcp:80".parse::<UpstreamSpec>().is_err());
        assert!("127.0.0.1/wtf".parse::<UpstreamSpec>().is_err());
        assert!("127.0.0.1:80/wtf".parse::<UpstreamSpec>().is_err());
        assert!("127.0.0.1:80/udp:80".parse::<UpstreamSpec>().is_err());
        assert!("127.0.0.1:80/udp/udp".parse::<UpstreamSpec>().is_err());
        assert!("example.com".parse::<UpstreamSpec>().is_err());
    }
}
