use ssh2;
use std::net::{self, TcpStream};

pub struct Session {
    ssh: ssh2::Session,
}

impl Session {
    pub(crate) fn connect<A: net::ToSocketAddrs>(addr: A) -> std::io::Result<Self> {
        let tcp = TcpStream::connect(addr)?;
        let mut sess = ssh2::Session::new()?;
        sess.set_tcp_stream(tcp); // session takes ownership of tcp stream/underlying socket
        sess.handshake()?;
        sess.userauth_pubkey_file(
            "ubuntu",
            None,
            std::path::Path::new("/home/neel/.ssh/rust_ec2_client.pem"),
            None,
        )
        .unwrap();
        Ok(Session { ssh: sess })
    }
}

use std::ops::{Deref, DerefMut};

impl Deref for Session {
    type Target = ssh2::Session;
    fn deref(&self) -> &Self::Target {
        &self.ssh
    }
}
impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ssh
    }
}
