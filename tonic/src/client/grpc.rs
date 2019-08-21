use crate::{
    body::{Body, BoxBody},
    codec::{decode_empty, decode_response, encode_client, Codec, Streaming},
    Code, GrpcService, Request, Response, Status,
};
use futures_core::Stream;
use futures_util::{future, stream, TryStreamExt};
use http::{
    header::{HeaderValue, CONTENT_TYPE, TE},
    uri::{Parts, PathAndQuery, Uri},
};
use http_body::Body as HttpBody;

pub struct Grpc<T> {
    inner: T,
}

impl<T> Grpc<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub async fn unary<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error> + Send,
        <T::ResponseBody as HttpBody>::Data: Send,
        C: Codec<Encode = M1, Decode = M2>,
        C::Encoder: Send + 'static,
        C::Decoder: Send + 'static,
        M1: Send + 'static,
        M2: Send + Unpin + 'static,
    {
        let request = request.map(|m| stream::once(future::ok(m)));
        self.client_streaming(request, path, codec).await
    }

    pub async fn client_streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error> + Send,
        <T::ResponseBody as HttpBody>::Data: Send,
        S: Stream<Item = Result<M1, Status>> + Send + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        C::Encoder: Send + 'static,
        C::Decoder: Send + 'static,
        M1: Send,
        M2: Send + Unpin + 'static,
    {
        let (parts, mut body) = self.streaming(request, path, codec).await?.into_parts();

        let message = body
            .try_next()
            .await?
            .ok_or(Status::new(Code::Internal, "Missing response message."))?;

        Ok(Response::from_parts(parts, message))
    }

    pub async fn server_streaming<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error> + Send,
        <T::ResponseBody as HttpBody>::Data: Send,
        C: Codec<Encode = M1, Decode = M2>,
        C::Encoder: Send + 'static,
        C::Decoder: Send + 'static,
        M1: Send + 'static,
        M2: Send + Unpin + 'static,
    {
        let request = request.map(|m| stream::once(future::ok(m)));
        self.streaming(request, path, codec).await
    }

    pub async fn streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        mut codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + HttpBody + Send + 'static,
        <T::ResponseBody as HttpBody>::Error: Into<crate::Error> + Send,
        <T::ResponseBody as HttpBody>::Data: Send,
        S: Stream<Item = Result<M1, Status>> + Send + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        C::Encoder: Send + 'static,
        C::Decoder: Send + 'static,
        M1: Send,
        M2: Send + Unpin + 'static,
    {
        let mut parts = Parts::default();
        parts.path_and_query = Some(path);

        let uri = Uri::from_parts(parts).expect("path_and_query only is valid Uri");

        let request = request
            .map(|s| encode_client(codec.encoder(), Box::pin(s)))
            .map(BoxBody::map_from);

        let mut request = request.into_http(uri);

        // Add the gRPC related HTTP headers
        request
            .headers_mut()
            .insert(TE, HeaderValue::from_static("trailers"));

        // Set the content type
        let content_type = <C as Codec>::CONTENT_TYPE;
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));

        let response = self
            .inner
            .call(request)
            .await
            .map_err(|err| Status::from_error(&*(err.into())))?;

        let status_code = response.status();
        let trailers_only_status = Status::from_header_map(response.headers());

        // We do not need to check for trailers if the `grpc-status` header is present
        // with a valid code.
        let expect_additional_trailers = if let Some(status) = trailers_only_status {
            if status.code() != Code::Ok {
                return Err(status);
            }

            false
        } else {
            true
        };

        let response = response
            .map(|b| {
                if expect_additional_trailers {
                    future::Either::Left(
                        decode_response(codec.decoder(), b, status_code).into_stream(),
                    )
                } else {
                    future::Either::Right(decode_empty(codec.decoder(), b).into_stream())
                }
            })
            .map(Streaming::new);

        Ok(Response::from_http(response))
    }
}

impl<T: Clone> Clone for Grpc<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
