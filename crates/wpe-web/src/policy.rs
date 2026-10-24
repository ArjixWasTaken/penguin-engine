//! Which hosts a page may reach: anything on the internet, but not this
//! machine or the local network (localhost, private and link-local
//! ranges, and names resolving to them) unless the user allows that host.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use url::{Host, Url};

#[derive(Clone, Debug, Default)]
pub struct NetworkPolicy {
    /// Hosts allowed even though they are local (`192.168.1.10`,
    /// `nas.lan`), compared case-insensitively.
    pub allowed_hosts: Vec<String>,
}

/// What to do with a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block,
    /// Allowed unless `host` resolves to a local address.
    Resolve(String, u16),
}

impl NetworkPolicy {
    /// The verdict on `url`. Local files are blocked here; the pages'
    /// own files and the files users pick are served by the host.
    pub fn check(&self, url: &Url) -> Verdict {
        match url.scheme() {
            "http" | "https" | "ws" | "wss" => {}
            "file" => return Verdict::Block,
            _ => return Verdict::Allow,
        }
        let Some(host) = url.host() else {
            return Verdict::Block;
        };
        let name = match &host {
            Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
            Host::Ipv4(ip) => ip.to_string(),
            Host::Ipv6(ip) => ip.to_string(),
        };
        if self
            .allowed_hosts
            .iter()
            .any(|h| h.trim_matches(['[', ']']).eq_ignore_ascii_case(&name))
        {
            return Verdict::Allow;
        }
        match host {
            Host::Ipv4(ip) if is_local(IpAddr::V4(ip)) => Verdict::Block,
            Host::Ipv6(ip) if is_local(IpAddr::V6(ip)) => Verdict::Block,
            Host::Ipv4(_) | Host::Ipv6(_) => Verdict::Allow,
            Host::Domain(_) if name == "localhost" || name.ends_with(".localhost") => {
                Verdict::Block
            }
            Host::Domain(_) => Verdict::Resolve(name, url.port_or_known_default().unwrap_or(80)),
        }
    }
}

/// Whether `host` resolves to a local address (or doesn't resolve).
pub fn resolves_local(host: &str, port: u16) -> bool {
    match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs.into_iter().any(|a| is_local(a.ip())),
        // Servo will fail it too.
        Err(_) => false,
    }
}

fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_local_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => is_local_v4(v4),
            None => is_local_v6(ip),
        },
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

fn is_local_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        // fc00::/7 unique local, fe80::/10 link-local.
        || first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(policy: &NetworkPolicy, url: &str) -> Verdict {
        policy.check(&Url::parse(url).unwrap())
    }

    #[test]
    fn local_addresses_are_blocked() {
        let p = NetworkPolicy::default();
        for url in [
            "http://127.0.0.1/",
            "http://127.4.5.6:8080/",
            "http://10.0.0.1/",
            "http://172.16.3.4/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://localhost:3000/",
            "ws://LOCALHOST./",
            "http://foo.localhost/",
        ] {
            assert_eq!(check(&p, url), Verdict::Block, "{url}");
        }
    }

    #[test]
    fn public_addresses_pass() {
        let p = NetworkPolicy::default();
        assert_eq!(check(&p, "https://8.8.8.8/"), Verdict::Allow);
        assert_eq!(check(&p, "http://172.32.0.1/"), Verdict::Allow);
        assert_eq!(check(&p, "http://[2001:db8::1]/"), Verdict::Allow);
        assert_eq!(
            check(&p, "https://Example.com/x"),
            Verdict::Resolve("example.com".into(), 443)
        );
        assert_eq!(check(&p, "data:text/plain,hi"), Verdict::Allow);
        assert_eq!(check(&p, "file:///etc/passwd"), Verdict::Block);
    }

    #[test]
    fn allowed_hosts_pass() {
        let p = NetworkPolicy {
            allowed_hosts: vec!["192.168.1.10".into(), "NAS.lan".into(), "[::1]".into()],
        };
        assert_eq!(check(&p, "http://192.168.1.10/"), Verdict::Allow);
        assert_eq!(check(&p, "http://nas.lan/"), Verdict::Allow);
        assert_eq!(check(&p, "http://[::1]:8000/"), Verdict::Allow);
        assert_eq!(check(&p, "http://192.168.1.11/"), Verdict::Block);
    }
}
