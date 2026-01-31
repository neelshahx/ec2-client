// no longer modern usage
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

// weird that this has no data
pub struct SshConnection;

pub struct Machine {
    ssh: Option<SshConnection>,
    instance_type: String,
    private_ip: String, // internal
    public_dns: String, // external
}

pub struct MachineSetup {
    instance_type: String,
    ami: String,
    setup: Box<dyn Fn(&mut SshConnection) -> io::Result<()>>, // short running function to run
                                                              // on startup, F is Box<dyn Fn>
                                                              // because func. can vary per machine
}

impl MachineSetup {
    pub fn new<F>(instance_type: &str, ami: &str, setup: F) -> Self
    where
        F: Fn(&mut SshConnection) -> io::Result<()> + 'static, // static b/c dont want fn to borrow
                                                               // objects with shorter lives
    {
        MachineSetup {
            instance_type: instance_type.to_string(),
            ami: ami.to_string(),
            setup: Box::new(setup),
        }
    }
}

pub struct BurstBuilder {
    descriptors: HashMap<String, (u32, MachineSetup)>, // u32 is num instances to launch
    max_duration: i64, // duration in minutes
}

impl Default for BurstBuilder {
    fn default() -> Self {
        BurstBuilder {
            descriptors: Default::default(),
            max_duration: 60,
        }
    }
}

// builder pattern
impl BurstBuilder {
    pub fn add_set(&mut self, name: &str, number: u32, setup: MachineSetup) {
        self.descriptors.insert(name.to_string(), (number, setup));
    }

    pub fn set_max_duration(&mut self, hours: i64) {
        self.max_duration = hours * 60;
    }

    pub async fn run<F>(self, f: F) // uses async
    where
        F: FnOnce(HashMap<String, &mut [Machine]>) -> io::Result<()>,
    {
        // make ec2 client
        let ec2 = rusoto_ec2::Ec2Client::new(rusoto_signature::Region::EuNorth1);

        // 1. issue spot requests
        let mut spot_req_ids = Vec::new();
        for (name, (number, setup)) in self.descriptors { // name -> num instancs, machine setup
            // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_RequestSpotLaunchSpecification.html
            let mut launch = rusoto_ec2::RequestSpotLaunchSpecification::default();
            launch.image_id = Some(setup.ami);
            launch.instance_type = Some(setup.instance_type);

            // static auth data
            launch.security_groups = Some(vec!["rust_ec2_client".to_string()]);
            launch.key_name = Some("rust_ec2_client".to_string());

            // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_RequestSpotInstances.html
            let mut req = rusoto_ec2::RequestSpotInstancesRequest::default();
            req.instance_count = Some(i64::from(number)); // number of instances to launch
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
        // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeSpotInstanceRequests.html
        let mut req = rusoto_ec2::DescribeSpotInstanceRequestsRequest::default();
        req.spot_instance_request_ids = Some(spot_req_ids.clone());
        let instances: Vec<_>;
        loop {
            let res = match ec2.describe_spot_instance_requests(req.clone()).await {
                Ok(res) => res,
                Err(_) => continue,
            };
            let any_open = res
                .spot_instance_requests
                .as_ref()
                .unwrap()
                .iter()
                .any(|sir| sir.state.as_ref().unwrap() == "open");
            if !any_open { //= some fulfilled, or closed or cancelled
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
        // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_CancelSpotInstanceRequests.html
        let mut cancel = rusoto_ec2::CancelSpotInstanceRequestsRequest::default();
        cancel.spot_instance_request_ids = spot_req_ids;
        ec2.cancel_spot_instance_requests(cancel).await.unwrap();

        // 4. wait until all instances are up and setups have been run
        let mut machines = Vec::new(); // ec2 provides private/public dns
        // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeInstances.html
        let mut desc_req = rusoto_ec2::DescribeInstancesRequest::default();
        desc_req.instance_ids = Some(instances);
        let mut all_ready = false;
        while !all_ready {
            all_ready = true;
            machines.clear();
            for reservation in ec2
                .describe_instances(desc_req.clone())
                .await
                .unwrap()
                .reservations
                .unwrap()
            {
                // if any reservation returns not as an instance, try again
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
                            all_ready = false;
                        }
                    }
                }
            }
        }

        //   - once an instance is ready, run to setup closure
        for machine in &machines {
            let tcp = loop {
                match TcpStream::connect(&format!("{}:22", machine.public_dns)) {
                    Ok(tcp) => break tcp,
                    Err(_) => continue,
                }
            };
            let mut sess = Session::new().unwrap();
            sess.set_tcp_stream(tcp);
            sess.handshake().unwrap();

            // sess.userauth_agent("ubuntu").unwrap(); // flaky
            sess.userauth_pubkey_file(
                "ubuntu",
                None,
                std::path::Path::new("/home/neel/.ssh/rust_ec2_client.pem"),
                None,
            ).unwrap();
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
        // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_TerminateInstances.html
        let mut termination_req = rusoto_ec2::TerminateInstancesRequest::default();
        termination_req.instance_ids = desc_req.instance_ids.unwrap();
        ec2.terminate_instances(termination_req).await.unwrap();
    }
}
