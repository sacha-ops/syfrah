# Test: invalid region/zone names are rejected by init and join

## Objective

invalid region/zone names are rejected by init and join.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- 2 servers with network connectivity on port 51820/UDP
- WireGuard kernel module loaded

## Steps

### 1. Testing: uppercase region

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region EU-WEST --zone eu-west-1a
```


### 2. Testing: leading dash in region

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region "-bad-region" --zone zone-1
```


### 3. Testing: trailing dash in region

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region "bad-region-" --zone zone-1
```


### 4. Testing: special characters in region

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region "eu_west" --zone zone-1
```


### 5. Testing: empty region

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region "" --zone zone-1
```


### 6. Testing: uppercase zone

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region eu-west --zone ZONE-A
```


### 7. Testing: leading dash in zone

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region eu-west --zone "-zone"
```


### 8. Testing: valid region/zone accepted

```bash
syfrah fabric init --name test-mesh --node-name node-1 --endpoint 172.20.0.10:51820 --region eu-west --zone eu-west-1a
```

```bash
syfrah fabric status
```


### 9. Testing: join with invalid region

```bash
syfrah fabric join 172.20.0.10:51821 --node-name node-2 --endpoint 172.20.0.11:51820 --pin "<PIN>" --region "BAD REGION" --zone zone-1
```


## Expected results

- uppercase region rejected
- leading dash region rejected
- trailing dash region rejected
- underscore in region rejected
- empty region rejected
- uppercase zone rejected
- leading dash zone rejected
- valid region accepted after rejections
- join with invalid region rejected

## Failure criteria

- uppercase region not rejected
- leading dash region not rejected
- trailing dash region not rejected
- underscore in region not rejected
- empty region not rejected
- uppercase zone not rejected
- leading dash zone not rejected
- valid init failed
- join with invalid region not rejected
