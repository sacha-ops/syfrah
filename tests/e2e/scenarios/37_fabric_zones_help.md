# Test: --region and --zone flags appear in CLI help

## Objective

--region and --zone flags appear in CLI help.

## Prerequisites

- A test server with `syfrah` installed and in PATH
- The syfrah daemon is NOT running (clean state)
- WireGuard kernel module loaded

## Steps

## Expected results

- init --help shows --region flag
- init --help shows --zone flag
- join --help shows --region flag
- join --help shows --zone flag

## Failure criteria

- init --help missing --region
- init --help missing --zone
- join --help missing --region
- join --help missing --zone
