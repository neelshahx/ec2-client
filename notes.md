### list and terminate instances

aws ec2 describe-instances --region eu-north-1 \
    --filters "Name=instance-state-name,Values=running,pending" \
    --query 'Reservations[*].Instances[*].[InstanceId,InstanceType,State.Name]' \
    --output table

aws ec2 terminate-instances --region eu-north-1 \
    --instance-ids $(aws ec2 describe-instances --region eu-north-1 \
    --filters "Name=instance-state-name,Values=running,pending" \
    --query 'Reservations[*].Instances[*].InstanceId' --output text)

### authenticating via ssh

ssh-add -l

aws ec2 delete-key-pair --region eu-north-1 --key-name rust_ec2_client

aws ec2 create-key-pair --region eu-north-1 \
--key-name rust_ec2_client \
--query 'KeyMaterial' \
--output text > ~/.ssh/rust_ec2_client.pem

chmod 400 ~/.ssh/rust_ec2_client.pem
ssh-add ~/.ssh/rust_ec2_client.pem

### find out what OS is running on ami (machine identifier), so you can choose correct username

aws ec2 describe-images --region eu-north-1 --image-ids ami-0014ce3e52359afbd --query 'Images[*].Name'
ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-20231207

- Amazon Linux: ec2-user
- Ubuntu: ubuntu
- Debian: admin
- CentOS: centos

# verify ssh connectivity on remove instance

aws ec2 describe-instances --region eu-north-1 \
--filters "Name=instance-state-name,Values=running" \
--query 'Reservations[*].Instances[*].PublicDnsName' --output text