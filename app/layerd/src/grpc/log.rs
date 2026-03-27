use tower::layer::Layer;
use tower::Service;
use tracing::info_span;

#[derive(Clone, Debug)]
pub struct LogLayer {
    target: &'static str,
}

impl LogLayer {
    pub fn new(target: &'static str) -> Self {
        LogLayer { target }
    }
}

impl<S> Layer<S> for LogLayer {
    type Service = LogService<S>;

    fn layer(&self, service: S) -> Self::Service {
        LogService {
            target: self.target,
            service,
        }
    }
}

// This service implements the Log behavior
#[derive(Clone, Debug)]
pub struct LogService<S> {
    target: &'static str,
    service: S,
}

impl<S, Request> Service<Request> for LogService<S>
where
    S: Service<Request>,
    Request: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Wrap actual call with a log statement
        let _span = info_span!(
            "service",
            target = self.target,
            request = tracing::field::debug(&request)
        );
        self.service.call(request)
    }
}
