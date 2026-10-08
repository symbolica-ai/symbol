//! Publishing and deleting sites and files, and archive downloads.

use std::io;
use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::api_error::ApiError;
use crate::app::App;
use crate::blob::add_target_expiry_headers;
use crate::headers::{
    CreationRequest, creation_request, filename_from, idempotency_from, if_match_from,
    insert_undo_headers, management_bearer, wants_replace, wants_unpack,
};
use crate::response::{created_or_ok, creation_response, plain};
use crate::secrets::ManagementToken;
use crate::store::{ArchiveFormat, CreationSecurity, Idempotency, PublishOptions, StoreError};
use crate::{store, upload};

pub struct TemporaryUpload {
    pub path: PathBuf,
}

#[derive(Clone, Copy)]
enum UploadLimitKind {
    Archive,
    File,
}

struct PublishRequestOptions {
    expected_tree_hash: Option<String>,
    idempotency: Option<Idempotency>,
    creation: CreationSecurity,
    authorization: Option<ManagementToken>,
    replace: bool,
}

impl PublishRequestOptions {
    fn as_options(&self) -> PublishOptions<'_> {
        PublishOptions {
            expected_tree_hash: self.expected_tree_hash.as_deref(),
            idempotency: self.idempotency.as_ref(),
            creation: self.creation,
            authorization: self.authorization.as_ref(),
            replace: self.replace,
        }
    }
}

impl UploadLimitKind {
    const fn error(self) -> upload::UploadError {
        match self {
            Self::Archive => upload::UploadError::ArchiveTooLarge,
            Self::File => upload::UploadError::FileTooLarge,
        }
    }
}

impl Drop for TemporaryUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Checks that `authorization` may mutate the existing site `name`.
async fn authorize_token(
    app: &App,
    name: &str,
    authorization: Option<&ManagementToken>,
) -> Result<(), StoreError> {
    let name = name.to_string();
    let authorization = authorization.cloned();
    app.run_store(move |store| store.authorize_mutation(&name, authorization.as_ref()))
        .await
}

/// Reads the bearer token and checks that it may mutate the site `name`.
pub async fn authorize(
    app: &App,
    name: &str,
    headers: &HeaderMap,
) -> Result<Option<ManagementToken>, ApiError> {
    let authorization = management_bearer(headers)?;
    authorize_token(app, name, authorization.as_ref()).await?;
    Ok(authorization)
}

pub async fn put_site_unnamed(
    State(app): State<App>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    publish(&app, None, &headers, body).await
}

pub async fn put_site(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    publish(&app, Some(name), &headers, body).await
}

async fn publish(
    app: &App,
    wanted: Option<String>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let authorization = management_bearer(headers)?;
    if let Some(name) = wanted.as_deref() {
        authorize_existing_site(app, name, headers, authorization.as_ref()).await?;
    }
    let (creation, options) = publish_request(headers, wanted.is_some(), authorization)?;
    let unpack = wants_unpack(headers);
    let upload = spool_and_sniff(app, headers, body, unpack).await?;
    let (name, mutation) = store_upload(app, wanted, unpack, upload, options).await?;
    let url = format!("{}/{name}/", app.public_url);
    Ok(creation_response(
        created_or_ok(&mutation),
        &url,
        format!(
            "ok {name} {url} ({} files, changed: {})",
            mutation.files, mutation.changed
        ),
        &mutation,
        creation,
        Some(mutation.sanitized),
    ))
}

/// A `PUT` to a site that already exists must be authorized, and cannot
/// claim management a second time.
async fn authorize_existing_site(
    app: &App,
    name: &str,
    headers: &HeaderMap,
    authorization: Option<&ManagementToken>,
) -> Result<(), ApiError> {
    let lookup = name.to_string();
    let exists = app
        .run_store(move |store| Ok(store.site_exists(&lookup)))
        .await?;
    if exists {
        if headers.contains_key("management-action") {
            return Err(StoreError::AlreadyManaged.into());
        }
        authorize_token(app, name, authorization).await?;
    }
    Ok(())
}

/// Validates the creation, precondition and replace headers of a site `PUT`.
fn publish_request(
    headers: &HeaderMap,
    named: bool,
    authorization: Option<ManagementToken>,
) -> Result<(CreationRequest, PublishRequestOptions), ApiError> {
    let creation = creation_request(headers)?;
    let expected_tree_hash = if_match_from(headers)?;
    let idempotency = (!named).then(|| idempotency_from(headers)).flatten();
    let replace = wants_replace(headers);
    if replace && !named {
        return Err("error: Replace applies to a named site PUT".into());
    }
    let options = PublishRequestOptions {
        expected_tree_hash,
        idempotency,
        creation: creation.security,
        authorization,
        replace,
    };
    Ok((creation, options))
}

/// A request body spooled to disk, with what its leading bytes say it is.
struct SniffedUpload {
    temporary: TemporaryUpload,
    kind: upload::Kind,
    filename: Option<String>,
}

async fn spool_and_sniff(
    app: &App,
    headers: &HeaderMap,
    body: Body,
    unpack: bool,
) -> Result<SniffedUpload, ApiError> {
    let filename = filename_from(headers);
    let ctype = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let (limit, limit_kind) = if unpack {
        (app.max_archive_upload, UploadLimitKind::Archive)
    } else {
        (app.max_file_size, UploadLimitKind::File)
    };
    let temporary = spool_body(app, headers, body, limit, limit_kind).await?;
    let prefix = read_prefix(&temporary.path).await?;
    let kind = upload::sniff(&prefix, ctype, filename.as_deref());
    if !unpack
        && matches!(
            kind,
            upload::Kind::Zip | upload::Kind::Tar | upload::Kind::Gzip
        )
    {
        let archive_path = temporary.path.clone();
        app.run_store(move |_| {
            upload::reject_secrets_in_opaque_archive(&archive_path, kind).map_err(StoreError::from)
        })
        .await?;
    }
    Ok(SniffedUpload {
        temporary,
        kind,
        filename,
    })
}

async fn store_upload(
    app: &App,
    wanted: Option<String>,
    unpack: bool,
    upload: SniffedUpload,
    options: PublishRequestOptions,
) -> Result<(String, store::MutationResult), StoreError> {
    let SniffedUpload {
        temporary,
        kind,
        filename,
    } = upload;
    if unpack {
        return publish_archive(app, wanted, filename, kind, temporary.path.clone(), options).await;
    }
    let stored_name = filename
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| kind.default_filename().to_string());
    app.run_store({
        let temporary = temporary.path.clone();
        move |store| {
            store.publish_uploaded_file(
                wanted.as_deref(),
                &stored_name,
                temporary,
                options.as_options(),
            )
        }
    })
    .await
}

async fn spool_body(
    app: &App,
    headers: &HeaderMap,
    body: Body,
    limit: u64,
    limit_kind: UploadLimitKind,
) -> Result<TemporaryUpload, StoreError> {
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|size| size > limit)
    {
        return Err(limit_kind.error().into());
    }
    let path = app.store.upload_path();
    let temporary = TemporaryUpload { path };
    let result = async {
        let mut file = tokio::fs::File::create(&temporary.path).await?;
        let mut stream = body.into_data_stream();
        let mut size = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| io::Error::other(err.to_string()))?;
            file.write_all(&chunk).await?;
            size += u64::try_from(chunk.len()).expect("body chunk size fits in u64");
            if size > limit {
                return Err(StoreError::Upload(limit_kind.error()));
            }
        }
        if size == 0 {
            return Err(StoreError::Upload(upload::UploadError::Empty));
        }
        file.sync_all().await?;
        Ok(())
    }
    .await;
    result?;
    Ok(temporary)
}

async fn read_prefix(path: &std::path::Path) -> io::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut prefix = vec![0_u8; 512];
    let read = file.read(&mut prefix).await?;
    prefix.truncate(read);
    Ok(prefix)
}

async fn publish_archive(
    app: &App,
    wanted: Option<String>,
    filename: Option<String>,
    kind: upload::Kind,
    path: PathBuf,
    options: PublishRequestOptions,
) -> Result<(String, store::MutationResult), StoreError> {
    app.run_store(move |store| {
        store.publish_uploaded_archive(
            wanted.as_deref(),
            filename.as_deref(),
            &path,
            kind,
            options.as_options(),
        )
    })
    .await
}

pub async fn put_file(
    State(app): State<App>,
    Path((name, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let authorization = authorize(&app, &name, &headers).await?;
    store::validate_mutation_target(&path)?;
    if wants_replace(&headers) {
        return Err("error: Replace applies to a whole site, not a single path".into());
    }
    let creation = creation_request(&headers)?;
    let expected_tree_hash = if_match_from(&headers)?;
    let temporary = spool_body(
        &app,
        &headers,
        body,
        app.max_file_size,
        UploadLimitKind::File,
    )
    .await?;
    let mutation = app
        .run_store({
            let name = name.clone();
            let path = path.clone();
            let temporary = temporary.path.clone();
            move |store| {
                store.put_uploaded_file_secured(
                    &name,
                    &path,
                    temporary,
                    PublishOptions {
                        expected_tree_hash: expected_tree_hash.as_deref(),
                        idempotency: None,
                        creation: creation.security,
                        authorization: authorization.as_ref(),
                        replace: false,
                    },
                )
            }
        })
        .await?;
    let url = format!("{}/{name}/{path}", app.public_url);
    Ok(creation_response(
        created_or_ok(&mutation),
        &url,
        format!("ok /{name}/{path} (changed: {})", mutation.changed),
        &mutation,
        creation,
        Some(mutation.sanitized),
    ))
}

pub async fn delete_site(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authorization = management_bearer(&headers)?;
    let request = archive_download(&name).unwrap_or(ArchiveDownload {
        name,
        format: ArchiveFormat::TarGz,
        extension: ".tar.gz",
        content_type: "application/gzip",
    });
    let temporary = TemporaryUpload {
        path: app.store.upload_path(),
    };
    let site_name = request.name.clone();
    let format = request.format;
    let pop = app
        .run_store({
            let path = temporary.path.clone();
            move |store| {
                store.pop_site_to_path_secured(&site_name, format, &path, authorization.as_ref())
            }
        })
        .await?;
    let mut response = archive_response(
        temporary,
        pop.size,
        request.content_type,
        format!(
            "attachment; filename=\"{}{}\"",
            request.name, request.extension
        ),
    )
    .await;
    insert_undo_headers(response.headers_mut(), &pop.undo);
    Ok(response)
}

pub async fn delete_file(
    State(app): State<App>,
    Path((name, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let authorization = authorize(&app, &name, &headers).await?;
    store::validate_mutation_target(&path)?;
    let mutation = app
        .run_store({
            let name = name.clone();
            let path = path.clone();
            move |store| store.delete_file_secured(&name, &path, authorization.as_ref())
        })
        .await?;
    let mut response = plain(StatusCode::OK, format!("deleted {name}/{path}"));
    if let Some(undo) = &mutation.undo {
        insert_undo_headers(response.headers_mut(), undo);
    }
    Ok(response)
}

#[derive(Clone)]
struct ArchiveDownload {
    name: String,
    format: ArchiveFormat,
    extension: &'static str,
    content_type: &'static str,
}

fn archive_download(path: &str) -> Option<ArchiveDownload> {
    [
        (".tar.gz", ArchiveFormat::TarGz, "application/gzip"),
        (".tar", ArchiveFormat::Tar, "application/x-tar"),
        (".zip", ArchiveFormat::Zip, "application/zip"),
    ]
    .into_iter()
    .find_map(|(extension, format, content_type)| {
        path.strip_suffix(extension).map(|name| ArchiveDownload {
            name: name.to_string(),
            format,
            extension,
            content_type,
        })
    })
}

async fn download_site(app: &App, request: ArchiveDownload) -> Result<Response, ApiError> {
    let temporary = TemporaryUpload {
        path: app.store.upload_path(),
    };
    let name = request.name.clone();
    let format = request.format;
    let size = app
        .run_store({
            let path = temporary.path.clone();
            move |store| store.pack_site_to_path(&name, format, &path)
        })
        .await?;
    let mut response = archive_response(
        temporary,
        size,
        request.content_type,
        format!(
            "attachment; filename=\"{}{}\"",
            request.name, request.extension
        ),
    )
    .await;
    add_target_expiry_headers(app, &request.name, "", response.headers_mut()).await;
    Ok(response)
}

async fn archive_response(
    temporary: TemporaryUpload,
    size: u64,
    content_type: &'static str,
    disposition: String,
) -> Response {
    let file = match tokio::fs::File::open(&temporary.path).await {
        Ok(file) => file,
        Err(err) => return StoreError::Io(err).into_response(),
    };
    let stream =
        futures_util::stream::try_unfold((file, temporary), |(mut file, temporary)| async move {
            let mut bytes = vec![0_u8; 64 * 1024];
            let read = file.read(&mut bytes).await?;
            if read == 0 {
                return Ok(None);
            }
            bytes.truncate(read);
            Ok::<_, io::Error>(Some((bytes::Bytes::from(bytes), (file, temporary))))
        });
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string()).expect("valid content length"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).expect("valid content disposition"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub async fn redirect_site(
    State(app): State<App>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    if let Some(request) = archive_download(&name) {
        return download_site(&app, request).await;
    }
    if name.contains('.') {
        return Err("error: unsupported archive suffix".into());
    }
    let location = format!("/{name}/");
    let exists = app
        .run_store(move |store| Ok(store.site_exists(&name)))
        .await?;
    if !exists {
        return Err(StoreError::NotFound.into());
    }
    Ok(Redirect::temporary(&location).into_response())
}
