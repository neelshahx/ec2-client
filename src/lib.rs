// no longer modern usage
extern crate rusoto_core;
extern crate rusoto_credential;
extern crate rusoto_ec2;
extern crate rusoto_signature;

use rusoto_ec2::Ec2;
use std::collections::HashMap;
use std::io;

// weird that this has no data
pub struct SshConnection;

mod ssh; // ties stream and session together

pub struct Machine {
    pub ssh: Option<ssh::Session>,
    pub instance_type: String,
    pub private_ip: String, // internal
    pub public_dns: String, // external
}

pub struct MachineSetup {
    instance_type: String,
    ami: String,
    setup: Box<dyn Fn(&mut ssh::Session) -> io::Result<()>>, // short running function to run
                                                             // on startup, F is Box<dyn Fn>
                                                             // because func. can vary per machine
}

impl MachineSetup {
    pub fn new<F>(instance_type: &str, ami: &str, setup: F) -> Self
    where
        F: Fn(&mut ssh::Session) -> io::Result<()> + 'static, // static b/c dont want fn to borrow
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
    max_duration: i64,                                 // duration in minutes
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

    pub async fn run<F>(self, f: F)
    // uses async
    where
        F: FnOnce(HashMap<String, Vec<Machine>>) -> io::Result<()>,
    {
        // make ec2 client
        let ec2 = rusoto_ec2::Ec2Client::new(rusoto_signature::Region::EuNorth1);

        let mut setup_fns = HashMap::new();

        // 1. issue spot requests
        let mut id_to_name = HashMap::new();
        let mut spot_req_ids = Vec::new();
        for (name, (number, setup)) in self.descriptors {
            // name -> num instancs, machine setup
            // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_RequestSpotLaunchSpecification.html
            let mut launch = rusoto_ec2::RequestSpotLaunchSpecification::default();
            launch.image_id = Some(setup.ami);
            launch.instance_type = Some(setup.instance_type);
            setup_fns.insert(name.clone(), setup.setup); // track closure for named group

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
                    .filter_map(|sir| sir.spot_instance_request_id)
                    .map(|sir| {
                        id_to_name.insert(sir.clone(), name.clone());
                        sir
                    }),
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
            let all_active = res
                .spot_instance_requests
                .as_ref()
                .unwrap()
                .iter()
                .all(|sir| sir.state.as_ref().unwrap() == "active");
            if all_active {
                instances = res
                    .spot_instance_requests
                    .unwrap()
                    .into_iter()
                    .filter_map(|sir| {
                        let name = id_to_name
                            .remove(&sir.spot_instance_request_id.unwrap())
                            .unwrap();
                        id_to_name.insert(sir.instance_id.as_ref().unwrap().clone(), name);
                        sir.instance_id
                    })
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
        let mut machines = HashMap::new(); // ec2 provides private/public dns
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
                            instance_id: Some(instance_id),
                            instance_type: Some(instance_type),
                            private_ip_address: Some(private_ip),
                            public_dns_name: Some(public_dns),
                            ..
                        } if !public_dns.is_empty() => {
                            let machine = Machine {
                                ssh: None,
                                instance_type,
                                private_ip,
                                public_dns,
                            };
                            let name = &id_to_name[&instance_id];
                            machines
                                .entry(name.clone())
                                .or_insert_with(Vec::new)
                                .push(machine);
                        }
                        _ => {
                            all_ready = false;
                        }
                    }
                }
            }
        }

        // once an instance is ready, run setup closure
        for (name, machines) in &mut machines {
            let f = &setup_fns[name];
            for machine in machines {
                let mut sess = loop {
                    match ssh::Session::connect(&format!("{}:22", machine.public_dns)) {
                        Ok(sess) => break sess,
                        Err(_) => continue,
                    }
                };
                f(&mut sess).unwrap();
                machine.ssh = Some(sess); // give ssh session to machine
            }
        }

        // 5. invoke F closure with Machine descriptors
        f(machines).unwrap();

        // 6. terminate all instances
        // https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_TerminateInstances.html
        let mut termination_req = rusoto_ec2::TerminateInstancesRequest::default();
        termination_req.instance_ids = desc_req.instance_ids.unwrap();
        // hack of ec2.terminate_instances(termination_req).await.unwrap();
        while let Err(e) = ec2.terminate_instances(termination_req.clone()).await {
            let msg = format!("{}", e);
            if msg.contains("Pooled stream disconnected") || msg.contains("broken pipe") {
                continue;
            } else {
                panic!("{}", msg);
            }
        }
    }
}
