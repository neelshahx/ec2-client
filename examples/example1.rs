extern crate ec2_client;

use ec2_client::{BurstBuilder, Machine, MachineSetup};
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    let mut b = BurstBuilder::default();
    b.add_set(
        "server",
        1,
        MachineSetup::new("t3.small", "ami-0014ce3e52359afbd", |sess| {
            use std::io::Read;
            let mut channel = sess.channel_session()?;
            channel.exec("cat /etc/hostname")?;
            let mut s = String::new();
            channel.read_to_string(&mut s)?;
            println!("{}", s);
            channel.wait_close()?;
            println!("{}", channel.exit_status()?);
            Ok(())
        }),
    );
    b.add_set(
        "client",
        2,
        MachineSetup::new("t3.small", "ami-0014ce3e52359afbd", |ssh| {
            // ssh.exec("sudo yum install htop")
            Ok(())
        }),
    );
    b.run(|vms: HashMap<String, &mut [Machine]>| {
        // let server_ip = vms["server"][0].ip;
        // let cmd = format!("ping {}", server_ip);
        // vms["client"].for_each_parallel(|client| {
        //     client.exec(cmd);
        // })
        Ok(())
    }).await;
}
