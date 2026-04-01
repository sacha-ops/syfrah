# Test: OpenTelemetry tracing

## Objective

Verify the OpenTelemetry tracing framework:
- OtelController with configurable OTLP endpoint (disabled by default)
- Span creation for API requests and reconciliation cycles
- Trace ID extraction from W3C traceparent header

## Steps

### 1. Unit test verification

```bash
cargo test -p syfrah-forge tracing_otel 2>&1
```

**Expected:** All tracing tests pass.

## Pass criteria

- OtelController exists with enable/disable support
- OTLP endpoint configurable (default: http://localhost:4317)
- Disabled by default (no runtime dependency on OTLP collector)
- extract_trace_id correctly parses W3C traceparent format
- Span macros for API requests and reconciliation cycles
