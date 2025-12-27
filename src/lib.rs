extern crate rusoto_core;
extern crate rusoto_credential;
extern crate rusoto_ec2;
extern crate rusoto_signature;
extern crate ssh2;

use rusoto_ec2::Ec2;
use ssh2::Session;
use std::collections::HashMap;
use std::io;
use std::io::Read;
use std::net::TcpStream;

pub struct SshConnection;

pub struct Machine {
    ssh: Option<SshConnection>,
    instance_type: String,
    private_ip: String,
    public_dns: String,
}

pub struct MachineSetup {
    instance_type: String,
    ami: String,
    setup: Box<dyn Fn(&mut SshConnection) -> io::Result<()>>,
}

impl MachineSetup {
    pub fn new<F>(instance_type: &str, ami: &str, setup: F) -> Self
    where
        F: Fn(&mut SshConnection) -> io::Result<()> + 'static,
    {
        MachineSetup {
            instance_type: instance_type.to_string(),
            ami: ami.to_string(),
            setup: Box::new(setup),
        }
    }
}

struct Burst;

pub struct BurstBuilder {
    descriptors: HashMap<String, (MachineSetup, u32)>,
    max_duration: i64,
}

impl Default for BurstBuilder {
    fn default() -> Self {
        BurstBuilder {
            descriptors: Default::default(),
            max_duration: 60,
        }
    }
}

impl BurstBuilder {
    pub fn add_set(&mut self, name: &str, number: u32, setup: MachineSetup) {
        self.descriptors.insert(name.to_string(), (setup, number));
    }

    pub fn set_max_duration(&mut self, hours: i64) {
        self.max_duration = hours * 60;
    }

    // TODO: crux
    pub async fn run<F>(self, f: F)
    where
        F: FnOnce(HashMap<String, &mut [Machine]>) -> io::Result<()>,
    {
        let ec2 = rusoto_ec2::Ec2Client::new(rusoto_signature::Region::EuNorth1);

        // 1. issue spot requests
        let mut spot_req_ids = Vec::new();
        for (name, (setup, number)) in self.descriptors {
            let mut launch = rusoto_ec2::RequestSpotLaunchSpecification::default();
            launch.image_id = Some(setup.ami);
            launch.instance_type = Some(setup.instance_type);

            launch.security_groups = Some(vec!["rust_ec2_client".to_string()]);
            launch.key_name = Some("rust_ec2_client".to_string());

            let mut req = rusoto_ec2::RequestSpotInstancesRequest::default();
            req.instance_count = Some(i64::from(number));
            // req.block_duration_minutes = Some(self.max_duration as i64);
            req.launch_specification = Some(launch);

            let res = ec2.request_spot_instances(req).await.unwrap();
            let res = res.spot_instance_requests.unwrap();

            spot_req_ids.extend(
                res.into_iter()
                    .filter_map(|sir| sir.spot_instance_request_id),
            );
        }

        // 2. wait for instances to come up
        let mut req = rusoto_ec2::DescribeSpotInstanceRequestsRequest::default();
        req.spot_instance_request_ids = Some(spot_req_ids.clone());
        let instances: Vec<_>;
        loop {
            let mut req = rusoto_ec2::DescribeSpotInstanceRequestsRequest::default();
            req.spot_instance_request_ids = Some(spot_req_ids.clone());

            let res = ec2.describe_spot_instance_requests(req).await.unwrap();
            let any_open = res
                .spot_instance_requests
                .as_ref()
                .unwrap()
                .iter()
                .any(|sir| sir.state.as_ref().unwrap() == "open");
            if !any_open {
                instances = res
                    .spot_instance_requests
                    .unwrap()
                    .into_iter()
                    .filter_map(|sir| sir.instance_id)
                    .collect();
                break;
            }
        }

        // 3. stop spot requests
        let mut cancel = rusoto_ec2::CancelSpotInstanceRequestsRequest::default();
        cancel.spot_instance_request_ids = spot_req_ids;
        ec2.cancel_spot_instance_requests(cancel).await.unwrap();

        // 4. wait until all instances are up and setups have been run
        let mut machines = Vec::new();
        let mut desc_req = rusoto_ec2::DescribeInstancesRequest::default();
        desc_req.instance_ids = Some(instances);
        let mut all_ready = false;
        while !all_ready {
            all_ready = true;
            machines.clear();

            let any_not_ready = false;
            for reservation in ec2
                .describe_instances(desc_req.clone())
                .await
                .unwrap()
                .reservations
                .unwrap()
            {
                for instance in reservation.instances.unwrap() {
                    match instance {
                        rusoto_ec2::Instance {
                            instance_type: Some(instance_type),
                            private_ip_address: Some(private_ip),
                            public_dns_name: Some(public_dns),
                            ..
                        } => {
                            let machine = Machine {
                                ssh: None,
                                instance_type,
                                private_ip,
                                public_dns,
                            };
                            machines.push(machine);
                        }
                        _ => {
                            all_ready = true;
                        }
                    }
                }
            }
        }

        //   - once an instance is ready, run to setup closure
        for machine in &machines {
            let tcp = TcpStream::connect(&format!("{}:22", machine.public_dns)).unwrap();
            let mut sess = Session::new().unwrap();
            sess.set_tcp_stream(tcp);
            sess.handshake().unwrap();

            sess.userauth_agent("ec2-user").unwrap();
            assert!(sess.authenticated());

            let mut channel = sess.channel_session().unwrap();
            channel.exec("cat /etc/hostname").unwrap();
            let mut s = String::new();
            channel.read_to_string(&mut s).unwrap();
            println!("{}", s);
            channel.wait_close().unwrap();
            println!("{}", channel.exit_status().unwrap());
        }

        // 5. invoke F closure with Machine dscriptors

        // 6. terminate all instances
        let mut termination_req = rusoto_ec2::TerminateInstancesRequest::default();
        termination_req.instance_ids = desc_req.instance_ids.unwrap();
        ec2.terminate_instances(termination_req).await.unwrap();
    }
}
