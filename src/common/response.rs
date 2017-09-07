// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Infrastructure types related to packaging rate-limit information alongside responses from
//! Twitter.

use std::{slice, vec, io, mem};
use std::iter::FromIterator;
use std::ops::{Deref, DerefMut};
use hyper::client::FutureResponse;
use hyper::{self, Body, StatusCode, Request};
use hyper::header::Headers;
use hyper_tls::HttpsConnector;
use tokio_core::reactor::Handle;
use futures::{Async, Future, Poll, Stream};
use rustc_serialize::json;
use super::{FromJson, field};
use error::{self, TwitterErrors};
use error::Error::*;

header! { (XRateLimitLimit, "X-Rate-Limit-Limit") => [i32] }
header! { (XRateLimitRemaining, "X-Rate-Limit-Remaining") => [i32] }
header! { (XRateLimitReset, "X-Rate-Limit-Reset") => [i32] }

///A helper struct to wrap response data with accompanying rate limit information.
///
///This is returned by any function that calls a rate-limited method on Twitter, to allow for
///inline checking of the rate-limit information without an extra call to
///`service::rate_limit_info`.
///
///As this implements `Deref` and `DerefMut`, you can transparently use the contained `response`'s
///methods as if they were methods on this struct.
#[derive(Debug)]
pub struct Response<T> {
    ///The rate limit ceiling for the given request.
    pub rate_limit: i32,
    ///The number of requests left for the 15-minute window.
    pub rate_limit_remaining: i32,
    ///The UTC Unix timestamp at which the rate window resets.
    pub rate_limit_reset: i32,
    ///The decoded response from the request.
    pub response: T,
}

impl<T> Response<T> {
    ///Convert a `Response<T>` to a `Response<U>` by running its contained response through the
    ///given function. This preserves its rate-limit information.
    ///
    ///Note that this is not a member function, so as to not conflict with potential methods on the
    ///contained `T`.
    pub fn map<F, U>(src: Response<T>, fun: F) -> Response<U>
        where F: FnOnce(T) -> U
    {
        Response {
            rate_limit: src.rate_limit,
            rate_limit_remaining: src.rate_limit_remaining,
            rate_limit_reset: src.rate_limit_reset,
            response: fun(src.response)
        }
    }
}

impl<T> Response<Vec<T>> {
    ///Returns an iterator that yields references into the returned collection, alongside
    ///rate-limit information for the whole method call.
    pub fn iter(&self) -> ResponseIterRef<T> {
        ResponseIterRef {
            rate_limit: self.rate_limit,
            rate_limit_remaining: self.rate_limit_remaining,
            rate_limit_reset: self.rate_limit_reset,
            resp_iter: self.response.iter(),
        }
    }

    ///Returns an iterator that yields mutable references into the returned collection, alongside
    ///rate-limit information for the whole method call.
    pub fn iter_mut(&mut self) -> ResponseIterMut<T> {
        ResponseIterMut {
            rate_limit: self.rate_limit,
            rate_limit_remaining: self.rate_limit_remaining,
            rate_limit_reset: self.rate_limit_reset,
            resp_iter: self.response.iter_mut(),
        }
    }
}

//This impl is used for service::rate_limit_status, to represent the individual method statuses
impl FromJson for Response<()> {
    fn from_json(input: &json::Json) -> Result<Self, error::Error> {
        if !input.is_object() {
            return Err(InvalidResponse("Response<()> received json that wasn't an object",
                                       Some(input.to_string())));
        }

        field_present!(input, limit);
        field_present!(input, remaining);
        field_present!(input, reset);

        Ok(Response {
            rate_limit: try!(field(input, "limit")),
            rate_limit_remaining: try!(field(input, "remaining")),
            rate_limit_reset: try!(field(input, "reset")),
            response: (),
        })
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

impl<T> DerefMut for Response<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.response
    }
}

///Iterator returned by calling `.iter()` on a `Response<Vec<T>>`.
///
///This provides a convenient method to iterate over a response that returned a collection, while
///copying rate-limit information across the entire iteration.
pub struct ResponseIterRef<'a, T> where T: 'a {
    rate_limit: i32,
    rate_limit_remaining: i32,
    rate_limit_reset: i32,
    resp_iter: slice::Iter<'a, T>,
}

impl<'a, T> Iterator for ResponseIterRef<'a, T> where T: 'a {
    type Item = Response<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.resp_iter.size_hint()
    }
}

impl<'a, T> DoubleEndedIterator for ResponseIterRef<'a, T> where T: 'a {
    fn next_back(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next_back() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }
}

impl<'a, T> ExactSizeIterator for ResponseIterRef<'a, T> where T: 'a {
    fn len(&self) -> usize {
        self.resp_iter.len()
    }
}

///Iteration over a response that returned a collection, while leaving the response in place.
impl<'a, T> IntoIterator for &'a Response<Vec<T>> where T: 'a {
    type Item = Response<&'a T>;
    type IntoIter = ResponseIterRef<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

///Iterator returned by calling `.iter_mut()` on a `Response<Vec<T>>`.
///
///This provides a convenient method to iterate over a response that returned a collection, while
///copying rate-limit information across the entire iteration.
pub struct ResponseIterMut<'a, T> where T: 'a {
    rate_limit: i32,
    rate_limit_remaining: i32,
    rate_limit_reset: i32,
    resp_iter: slice::IterMut<'a, T>,
}

impl<'a, T> Iterator for ResponseIterMut<'a, T> where T: 'a {
    type Item = Response<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.resp_iter.size_hint()
    }
}

impl<'a, T> DoubleEndedIterator for ResponseIterMut<'a, T> where T: 'a {
    fn next_back(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next_back() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }
}

impl<'a, T> ExactSizeIterator for ResponseIterMut<'a, T> where T: 'a {
    fn len(&self) -> usize {
        self.resp_iter.len()
    }
}

///Mutable iteration over a response that returned a collection, while leaving the response in
///place.
impl<'a, T> IntoIterator for &'a mut Response<Vec<T>> where T: 'a {
    type Item = Response<&'a mut T>;
    type IntoIter = ResponseIterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

///Iterator returned by calling `.into_iter()` on a `Response<Vec<T>>`.
///
///This provides a convenient method to iterate over a response that returned a collection, while
///copying rate-limit information across the entire iteration. For example, this is used in
///`CursorIter`'s implemention to propagate rate-limit information across a given page of results.
pub struct ResponseIter<T> {
    rate_limit: i32,
    rate_limit_remaining: i32,
    rate_limit_reset: i32,
    resp_iter: vec::IntoIter<T>,
}

impl<T> Iterator for ResponseIter<T> {
    type Item = Response<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.resp_iter.size_hint()
    }
}

impl<T> DoubleEndedIterator for ResponseIter<T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if let Some(resp) = self.resp_iter.next_back() {
            Some(Response {
                rate_limit: self.rate_limit,
                rate_limit_remaining: self.rate_limit_remaining,
                rate_limit_reset: self.rate_limit_reset,
                response: resp,
            })
        }
        else {
            None
        }
    }
}

impl<T> ExactSizeIterator for ResponseIter<T> {
    fn len(&self) -> usize {
        self.resp_iter.len()
    }
}

///Iteration over a response that returned a collection, copying the rate limit information across
///all values.
impl<T> IntoIterator for Response<Vec<T>> {
    type Item = Response<T>;
    type IntoIter = ResponseIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        ResponseIter {
            rate_limit: self.rate_limit,
            rate_limit_remaining: self.rate_limit_remaining,
            rate_limit_reset: self.rate_limit_reset,
            resp_iter: self.response.into_iter(),
        }
    }
}

///`FromIterator` impl that allows collecting several responses into one, preserving the latest
///rate limit information.
impl<T> FromIterator<Response<T>> for Response<Vec<T>> {
    fn from_iter<I>(iter: I) -> Self
        where I: IntoIterator<Item=Response<T>>
    {
        let mut resp = Response {
            rate_limit: -1,
            rate_limit_remaining: -1,
            rate_limit_reset: -1,
            response: Vec::new(),
        };

        for item in iter {
            if item.rate_limit_reset > resp.rate_limit_reset {
                resp.rate_limit = item.rate_limit;
                resp.rate_limit_remaining = item.rate_limit_remaining;
                resp.rate_limit_reset = item.rate_limit_reset;
            }
            else if (item.rate_limit_reset == resp.rate_limit_reset) &&
                    (item.rate_limit_remaining < resp.rate_limit_remaining) {
                resp.rate_limit = item.rate_limit;
                resp.rate_limit_remaining = item.rate_limit_remaining;
                resp.rate_limit_reset = item.rate_limit_reset;
            }
            resp.response.push(item.response);
        }

        resp
    }
}

/// A `Future` that resolves a web request and loads the complete response into a String.
///
/// This also does some header inspection, and attempts to parse the response as a `TwitterErrors`
/// before returning the String.
pub struct RawFuture<'a> {
    handle: &'a Handle,
    request: Option<Request>,
    response: Option<FutureResponse>,
    resp_headers: Option<Headers>,
    resp_status: Option<StatusCode>,
    body_stream: Option<Body>,
    body: Vec<u8>,
}

impl<'a> RawFuture<'a> {
    fn headers(&self) -> &Headers {
        self.resp_headers.as_ref().unwrap()
    }
}

impl<'a> Future for RawFuture<'a> {
    type Item = String;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(req) = self.request.take() {
            // needed to pull this section into the future so i could try!() on the connector
            // TODO: num-cpus?
            let connector = try!(HttpsConnector::new(1, self.handle));
            let client = hyper::Client::configure().connector(connector).build(self.handle);
            self.response = Some(client.request(req));
        }

        if let Some(mut resp) = self.response.take() {
            match resp.poll() {
                Err(e) => return Err(e.into()),
                Ok(Async::NotReady) => {
                    self.response = Some(resp);
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(resp)) => {
                    self.resp_headers = Some(resp.headers().clone());
                    self.resp_status = Some(resp.status());
                    self.body_stream = Some(resp.body());
                }
            }
        }

        if let Some(mut resp) = self.body_stream.take() {
            match resp.poll() {
                Err(e) => return Err(e.into()),
                Ok(Async::NotReady) => {
                    self.body_stream = Some(resp);
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(Some(chunk))) => {
                    self.body.extend(&*chunk);
                    self.body_stream = Some(resp);
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(None)) => { }
            }
        }

        match String::from_utf8(mem::replace(&mut self.body, Vec::new())) {
            Err(_) => Err(io::Error::new(io::ErrorKind::InvalidData,
                                         "stream did not contain valid UTF-8").into()),
            Ok(resp) => {
                if let Ok(err) = json::decode::<TwitterErrors>(&resp) {
                    if err.errors.iter().any(|e| e.code == 88) &&
                        self.headers().has::<XRateLimitReset>()
                    {
                        return Err(
                            RateLimit(
                                self.headers().get::<XRateLimitReset>().map(|h| h.0).unwrap()
                            )
                        );
                    }
                    else {
                        return Err(TwitterError(err));
                    }
                }

                match self.resp_status.unwrap() {
                    StatusCode::Ok => Ok(Async::Ready(resp)),
                    st => Err(BadStatus(st)),
                }
            }
        }
    }
}

/// Creates a new `RawFuture` starting with the given `Request`, to be run on the Core represented
/// by the given `Handle`.
fn make_raw_future<'a>(handle: &'a Handle, request: Request) -> RawFuture<'a> {
    RawFuture {
        handle: handle,
        request: Some(request),
        response: None,
        resp_headers: None,
        resp_status: None,
        body_stream: None,
        body: Vec::new(),
    }
}

/// Helper trait to get around `Box<FnOnce>` being unusable and `FnBox` being unstable.
pub trait MakeResponse<T> {
    fn make_response(self: Box<Self>, raw: String, headers: &Headers) -> Result<T, error::Error>;
}

impl<T, F> MakeResponse<T> for F
    where F: FnOnce(String, &Headers) -> Result<T, error::Error>
{
    fn make_response(self: Box<Self>, raw: String, headers: &Headers) -> Result<T, error::Error> {
        (*self)(raw, headers)
    }
}

/// A `Future` that will resolve to a complete Twitter response.
///
/// When this `Future` is fully complete, the pending web request will have successfully completed,
/// loaded, and parsed into the desired response. Any errors encountered along the way will be
/// reflected in the return type of `poll`.
///
/// For more information on how to use `Future`s, see the guides at [hyper.rs] and [tokio.rs].
///
/// [hyper.rs]: https://hyper.rs/guides/
/// [tokio.rs]: https://tokio.rs/docs/getting-started/tokio/
#[must_use = "futures do nothing unless polled"]
pub struct TwitterFuture<'a, T> {
    request: RawFuture<'a>,
    make_resp: Option<Box<MakeResponse<T> + 'a>>,
}

impl<'a, T> Future for TwitterFuture<'a, T> {
    type Item = T;
    type Error = error::Error;

     fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
         let full_resp = match self.request.poll() {
             Err(e) => return Err(e),
             Ok(Async::NotReady) => return Ok(Async::NotReady),
             Ok(Async::Ready(r)) => r,
         };

         if let Some(make_resp) = self.make_resp.take() {
             Ok(Async::Ready(try!(make_resp.make_response(full_resp, self.request.headers()))))
         } else {
             Err(io::Error::new(io::ErrorKind::Other,
                                "response has already been processed").into())
         }
     }
}

/// Create a `TwitterFuture` that processes the response through the given function before
/// returning.
pub fn make_future<'a, T, F>(handle: &'a Handle, request: Request, make_resp: F)
    -> TwitterFuture<'a, T>
    where F: MakeResponse<T> + 'a
{
    TwitterFuture {
        request: make_raw_future(handle, request),
        make_resp: Some(Box::new(make_resp)),
    }
}

/// Shortcut `MakeResponse` method that attempts to parse the given type from the response and
/// loads rate-limit information from the response headers.
pub fn make_response<T: FromJson>(full_resp: String, headers: &Headers)
    -> Result<Response<T>, error::Error>
{
    let out = try!(T::from_str(&full_resp));

    Ok(Response::map(rate_headers(headers), |_| out))
}

/// Shortcut function to create a `TwitterFuture` that parses out the given type from its response.
pub fn make_parsed_future<'a, T: FromJson + 'a>(handle: &'a Handle, request: Request)
    -> TwitterFuture<'a, Response<T>>
{
    make_future(handle, request, make_response)
}

pub fn rate_headers(resp: &Headers) -> Response<()> {
    Response {
        rate_limit: resp.get::<XRateLimitLimit>().map_or(-1, |h| h.0),
        rate_limit_remaining: resp.get::<XRateLimitRemaining>().map_or(-1, |h| h.0),
        rate_limit_reset: resp.get::<XRateLimitReset>().map_or(-1, |h| h.0),
        response: (),
    }
}
