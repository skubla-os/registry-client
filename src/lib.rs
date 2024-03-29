#![allow(unknown_lints)]
#![allow(clippy::identity_op)] // used for vertical alignment

use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::*;
use std::io::Cursor;

use curl::easy::{Easy, List};
use failure::bail;
use http::status::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json;
use url::percent_encoding::{percent_encode, QUERY_ENCODE_SET};

pub type Result<T> = std::result::Result<T, failure::Error>;

pub struct Registry {
    /// The base URL for issuing API requests.
    host: String,
    /// Optional authorization token.
    /// If None, commands requiring authorization will fail.
    token: Option<String>,
    /// Curl handle for issuing requests.
    handle: Easy,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Auth {
    Authorized,
    Unauthorized,
}

#[derive(Deserialize)]
pub struct Package {
    pub name: String,
    pub description: Option<String>,
    pub max_version: String,
}

#[derive(Serialize)]
pub struct NewPackage {
    pub name: String,
    pub vers: String,
    pub deps: Vec<NewPackageDependency>,
    pub features: BTreeMap<String, Vec<String>>,
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub documentation: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
    pub readme_file: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub repository: Option<String>,
    pub badges: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub links: Option<String>,
}

#[derive(Serialize)]
pub struct NewPackageDependency {
    pub optional: bool,
    pub default_features: bool,
    pub name: String,
    pub features: Vec<String>,
    pub version_req: String,
    pub target: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_name_in_toml: Option<String>,
}

#[derive(Deserialize)]
pub struct User {
    pub id: u32,
    pub login: String,
    pub avatar: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
}

pub struct Warnings {
    pub invalid_categories: Vec<String>,
    pub invalid_badges: Vec<String>,
    pub other: Vec<String>,
}

#[derive(Deserialize)]
struct R {
    ok: bool,
}
#[derive(Deserialize)]
struct OwnerResponse {
    ok: bool,
    msg: String,
}
#[derive(Deserialize)]
struct ApiErrorList {
    errors: Vec<ApiError>,
}
#[derive(Deserialize)]
struct ApiError {
    detail: String,
}
#[derive(Serialize)]
struct OwnersReq<'a> {
    users: &'a [&'a str],
}
#[derive(Deserialize)]
struct Users {
    users: Vec<User>,
}
#[derive(Deserialize)]
struct TotalPackages {
    total: u32,
}
#[derive(Deserialize)]
struct Packages {
    packages: Vec<Package>,
    meta: TotalPackages,
}
impl Registry {
    pub fn new(host: String, token: Option<String>) -> Registry {
        Registry::new_handle(host, token, Easy::new())
    }

    pub fn new_handle(host: String, token: Option<String>, handle: Easy) -> Registry {
        Registry {
            host,
            token,
            handle,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn add_owners(&mut self, pkg: &str, owners: &[&str]) -> Result<String> {
        let body = serde_json::to_string(&OwnersReq { users: owners })?;
        let body = self.put(&format!("/packages/{}/owners", pkg), body.as_bytes())?;
        assert!(serde_json::from_str::<OwnerResponse>(&body)?.ok);
        Ok(serde_json::from_str::<OwnerResponse>(&body)?.msg)
    }

    pub fn remove_owners(&mut self, pkg: &str, owners: &[&str]) -> Result<()> {
        let body = serde_json::to_string(&OwnersReq { users: owners })?;
        let body = self.delete(&format!("/packages/{}/owners", pkg), Some(body.as_bytes()))?;
        assert!(serde_json::from_str::<OwnerResponse>(&body)?.ok);
        Ok(())
    }

    pub fn list_owners(&mut self, pkg: &str) -> Result<Vec<User>> {
        let body = self.get(&format!("/packages/{}/owners", pkg))?;
        Ok(serde_json::from_str::<Users>(&body)?.users)
    }

    pub fn publish(&mut self, pkg: &NewPackage, tarball: &File) -> Result<Warnings> {
        let json = serde_json::to_string(pkg)?;
        // Prepare the body. The format of the upload request is:
        //
        //      <le u32 of json>
        //      <json request> (metadata for the package)
        //      <le u32 of tarball>
        //      <source tarball>
        let stat = tarball.metadata()?;
        let header = {
            let mut w = Vec::new();
            w.extend(
                [
                    (json.len() >> 0) as u8,
                    (json.len() >> 8) as u8,
                    (json.len() >> 16) as u8,
                    (json.len() >> 24) as u8,
                ].iter().cloned(),
            );
            w.extend(json.as_bytes().iter().cloned());
            w.extend(
                [
                    (stat.len() >> 0) as u8,
                    (stat.len() >> 8) as u8,
                    (stat.len() >> 16) as u8,
                    (stat.len() >> 24) as u8,
                ].iter().cloned(),
            );
            w
        };
        let size = stat.len() as usize + header.len();
        let mut body = Cursor::new(header).chain(tarball);

        let url = format!("{}/api/v1/packages/new", self.host);

        let token = match self.token.as_ref() {
            Some(s) => s,
            None => bail!("no upload token found, please run `nianjia login`"),
        };
        self.handle.put(true)?;
        self.handle.url(&url)?;
        self.handle.in_filesize(size as u64)?;
        let mut headers = List::new();
        headers.append("Accept: application/json")?;
        headers.append(&format!("Authorization: {}", token))?;
        self.handle.http_headers(headers)?;

        let body = handle(&mut self.handle, &mut |buf| body.read(buf).unwrap_or(0))?;

        let response = if body.is_empty() {
            "{}".parse()?
        } else {
            body.parse::<serde_json::Value>()?
        };

        let invalid_categories: Vec<String> = response
            .get("warnings")
            .and_then(|j| j.get("invalid_categories"))
            .and_then(|j| j.as_array())
            .map(|x| x.iter().flat_map(|j| j.as_str()).map(Into::into).collect())
            .unwrap_or_else(Vec::new);

        let invalid_badges: Vec<String> = response
            .get("warnings")
            .and_then(|j| j.get("invalid_badges"))
            .and_then(|j| j.as_array())
            .map(|x| x.iter().flat_map(|j| j.as_str()).map(Into::into).collect())
            .unwrap_or_else(Vec::new);

        let other: Vec<String> = response
            .get("warnings")
            .and_then(|j| j.get("other"))
            .and_then(|j| j.as_array())
            .map(|x| x.iter().flat_map(|j| j.as_str()).map(Into::into).collect())
            .unwrap_or_else(Vec::new);

        Ok(Warnings {
            invalid_categories,
            invalid_badges,
            other,
        })
    }

    pub fn search(&mut self, query: &str, limit: u32) -> Result<(Vec<Package>, u32)> {
        let formatted_query = percent_encode(query.as_bytes(), QUERY_ENCODE_SET);
        let body = self.req(
            &format!("/packages?q={}&per_page={}", formatted_query, limit),
            None,
            Auth::Unauthorized,
        )?;

        let packages = serde_json::from_str::<Packages>(&body)?;
        Ok((packages.packages, packages.meta.total))
    }

    pub fn yank(&mut self, pkg: &str, version: &str) -> Result<()> {
        let body = self.delete(&format!("/packages/{}/{}/yank", pkg, version), None)?;
        assert!(serde_json::from_str::<R>(&body)?.ok);
        Ok(())
    }

    pub fn unyank(&mut self, pkg: &str, version: &str) -> Result<()> {
        let body = self.put(&format!("/packages/{}/{}/unyank", pkg, version), &[])?;
        assert!(serde_json::from_str::<R>(&body)?.ok);
        Ok(())
    }

    fn put(&mut self, path: &str, b: &[u8]) -> Result<String> {
        self.handle.put(true)?;
        self.req(path, Some(b), Auth::Authorized)
    }

    fn get(&mut self, path: &str) -> Result<String> {
        self.handle.get(true)?;
        self.req(path, None, Auth::Authorized)
    }

    fn delete(&mut self, path: &str, b: Option<&[u8]>) -> Result<String> {
        self.handle.custom_request("DELETE")?;
        self.req(path, b, Auth::Authorized)
    }

    fn req(&mut self, path: &str, body: Option<&[u8]>, authorized: Auth) -> Result<String> {
        self.handle.url(&format!("{}/api/v1{}", self.host, path))?;
        let mut headers = List::new();
        headers.append("Accept: application/json")?;
        headers.append("Content-Type: application/json")?;

        if authorized == Auth::Authorized {
            let token = match self.token.as_ref() {
                Some(s) => s,
                None => bail!("no upload token found, please run `nianjia login`"),
            };
            headers.append(&format!("Authorization: {}", token))?;
        }
        self.handle.http_headers(headers)?;
        match body {
            Some(mut body) => {
                self.handle.upload(true)?;
                self.handle.in_filesize(body.len() as u64)?;
                handle(&mut self.handle, &mut |buf| body.read(buf).unwrap_or(0))
            }
            None => handle(&mut self.handle, &mut |_| 0),
        }
    }
}

fn handle(handle: &mut Easy, read: &mut dyn FnMut(&mut [u8]) -> usize) -> Result<String> {
    let mut headers = Vec::new();
    let mut body = Vec::new();
    {
        let mut handle = handle.transfer();
        handle.read_function(|buf| Ok(read(buf)))?;
        handle.write_function(|data| {
            body.extend_from_slice(data);
            Ok(data.len())
        })?;
        handle.header_function(|data| {
            headers.push(String::from_utf8_lossy(data).into_owned());
            true
        })?;
        handle.perform()?;
    }

    let body = match String::from_utf8(body) {
        Ok(body) => body,
        Err(..) => bail!("response body was not valid utf-8"),
    };
    let errors = serde_json::from_str::<ApiErrorList>(&body).ok().map(|s| {
        s.errors.into_iter().map(|s| s.detail).collect::<Vec<_>>()
    });

    match (handle.response_code()?, errors) {
        (0, None) | (200, None) => {},
        (code, Some(errors)) => {
            let code = StatusCode::from_u16(code as _)?;
            bail!("api errors (status {}): {}", code, errors.join(", "))
        }
        (code, None) => bail!(
            "failed to get a 200 OK response, got {}\n\
             headers:\n\
             \t{}\n\
             body:\n\
             {}",
             code,
             headers.join("\n\t"),
             body,
        ),
    }

    Ok(body)
}
