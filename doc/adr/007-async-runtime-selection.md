# ADR-007: Async Runtime Selection

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Use tokio for async runtime because it's industry standard for Rust async, has largest ecosystem, and best debugging tools.

## Decision

**Use tokio as async runtime.**

## Rationale

**Why tokio?**

- Industry standard (used by AWS, Discord, Cloudflare)
- Largest ecosystem (10,000+ crates depend on it)
- Best GPU integration (NVIDIA research on CUDA+tokio)
- Excellent debugging (tokio-console)
- Mature (since 2016)

**Why not async-std?**

- Smaller ecosystem
- Less GPU integration research
- Harder to debug

**Why not smol?**

- Embedded-focused (too minimal for HPC)
- Less mature

## Implementation

```rust
use tokio::sync::mpsc;
use tokio::task;

pub struct Channel {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl Channel {
    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        self.tx.send(frame).await
            .map_err(|_| Error::ChannelClosed)?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Frame> {
        self.rx.recv().await
            .ok_or(Error::ChannelClosed)
    }
}
```

## Consequences

 Leverage largest Rust async ecosystem Future GPU-aware async primitives Dependency on external crate (acceptable
 trade-off)
