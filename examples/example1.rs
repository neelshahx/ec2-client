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
            sess.command("cat /etc/hostname").map(|out| println!("{}", out))
        }),
    );
    b.add_set(
        "client",
        2,
        MachineSetup::new("t3.small", "ami-0014ce3e52359afbd", |sess| {
            sess.command("date").map(|out| println!("{}", out))
        }),
    );
    b.run(|vms: HashMap<String, Vec<Machine>>| {
        println!("{}", vms["server"][0].private_ip);
        println!("{}", vms["client"][0].private_ip);
        println!("{}", vms["client"][1].private_ip);
        Ok(())
    }).await;
}
