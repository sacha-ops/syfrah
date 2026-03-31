# 283 - VM create --subnet flag

## Purpose

Verify that `syfrah compute vm create` correctly resolves the `--subnet` flag,
auto-selects when an environment has exactly one subnet, and errors with helpful
guidance when zero or multiple subnets exist.

## Prerequisites

- A running daemon (`syfrah fabric init --name test-mesh`)
- Org/project/env/subnet hierarchy created

## Setup

```bash
syfrah org create acme
syfrah project create backend --org acme
syfrah env create production --project backend --org acme
```

## Scenario 1: Auto-select single subnet

```bash
syfrah subnet create frontend --env production --project backend --org acme
syfrah compute vm create --name web-1 --image alpine-3.20 \
  --env production --project backend --org acme
```

**Expected:** VM created successfully, subnet auto-selected (frontend).

## Scenario 2: Explicit --subnet with multiple subnets

```bash
syfrah subnet create database --env production --project backend --org acme
syfrah compute vm create --name db-1 --image alpine-3.20 \
  --subnet database --env production --project backend --org acme
```

**Expected:** VM created successfully using the `database` subnet.

## Scenario 3: Error when multiple subnets and no --subnet

```bash
syfrah compute vm create --name api-1 --image alpine-3.20 \
  --env production --project backend --org acme
```

**Expected:** Error message listing available subnets:
`environment 'production' has multiple subnets: frontend, database. Specify --subnet`

## Scenario 4: Error when no subnets exist

```bash
syfrah env create staging --project backend --org acme
syfrah compute vm create --name stg-1 --image alpine-3.20 \
  --env staging --project backend --org acme
```

**Expected:** Error with creation guidance:
`no subnet found for environment 'staging'. Create one with: syfrah subnet create <name> --env staging --project backend --org acme`

## Scenario 5: Error when subnet name doesn't exist

```bash
syfrah compute vm create --name web-2 --image alpine-3.20 \
  --subnet nonexistent --env production --project backend --org acme
```

**Expected:** Error:
`subnet 'nonexistent' not found in environment 'production'. Create one with: syfrah subnet create nonexistent --env production --project backend --org acme`

## Scenario 6: Partial org context errors

```bash
syfrah compute vm create --name web-3 --image alpine-3.20 --env production
```

**Expected:** Error: `--env, --project, and --org must all be specified together`

## Scenario 7: --subnet without org context errors

```bash
syfrah compute vm create --name web-4 --image alpine-3.20 --subnet frontend
```

**Expected:** Error: `--subnet requires --env, --project, and --org to resolve the subnet`

## Teardown

```bash
syfrah compute vm delete web-1 --yes
syfrah compute vm delete db-1 --yes
syfrah subnet delete frontend --yes
syfrah subnet delete database --yes
syfrah env destroy production --project backend --org acme --yes
syfrah env destroy staging --project backend --org acme --yes
syfrah project delete backend --org acme --yes
syfrah org delete acme --yes
```
