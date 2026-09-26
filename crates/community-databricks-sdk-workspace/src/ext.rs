//! Workspace file helpers from Go's `service/workspace/ext_utilities.go`:
//! `upload`, `download`, `read_file`, `write_file`, `recursive_list`, the
//! Python notebook `Import` builders and `ExportResponse::bytes`.

use std::collections::VecDeque;

use community_databricks_core::http::{Binary, Bytes, Call, Method, idempotency_token};
use community_databricks_core::text::{base64_decode, base64_encode, trim_leading_whitespace};
use community_databricks_core::{Error, Result};

use crate::{
    ExportFormat, ExportResponse, Import, ImportFormat, Language, ListWorkspaceRequest, ObjectInfo,
    ObjectType, WorkspaceApi,
};

/// Options for [`WorkspaceApi::upload`] (Go: `UploadOverwrite`,
/// `UploadLanguage`, `UploadFormat`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadOptions {
    /// Replace an existing object.
    pub overwrite: bool,
    /// Notebook language. When unset, and the format is `SOURCE` or unset,
    /// it is inferred from the extension (`.py`, `.sql`, `.scala`, `.R`).
    /// Go infers even when a language is given; an explicit one wins here.
    pub language: Option<Language>,
    /// Import format; the server's default when unset.
    pub format: Option<ImportFormat>,
}

impl UploadOptions {
    /// Replace an existing object (Go: `UploadOverwrite`).
    #[must_use]
    pub fn overwrite(mut self) -> Self {
        self.overwrite = true;
        self
    }

    /// Set the notebook language (Go: `UploadLanguage`).
    #[must_use]
    pub fn language(mut self, language: Language) -> Self {
        self.language = Some(language);
        self
    }

    /// Set the import format (Go: `UploadFormat`).
    #[must_use]
    pub fn format(mut self, format: ImportFormat) -> Self {
        self.format = Some(format);
        self
    }
}

/// Options for [`WorkspaceApi::download`] (Go: `DownloadFormat`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DownloadOptions {
    /// Export format; the server's default when unset.
    pub format: Option<ExportFormat>,
}

impl DownloadOptions {
    /// Set the export format (Go: `DownloadFormat`).
    #[must_use]
    pub fn format(mut self, format: ExportFormat) -> Self {
        self.format = Some(format);
        self
    }
}

impl Import {
    /// An import that overwrites `path` with a Python notebook whose source
    /// is `content`, after removing its common indentation (Go:
    /// `PythonNotebookOverwrite`).
    #[must_use]
    pub fn python_notebook_overwrite(path: impl Into<String>, content: &str) -> Self {
        Self::python_notebook_overwrite_bytes(path, trim_leading_whitespace(content).as_bytes())
    }

    /// As [`python_notebook_overwrite`](Self::python_notebook_overwrite),
    /// from raw bytes and without trimming (Go:
    /// `PythonNotebookOverwriteReader`).
    #[must_use]
    pub fn python_notebook_overwrite_bytes(path: impl Into<String>, content: &[u8]) -> Self {
        Self::new(path)
            .with_overwrite(true)
            .with_format(ImportFormat::Source)
            .with_language(Language::Python)
            .with_content(base64_encode(content))
    }
}

impl ExportResponse {
    /// The exported content, base64-decoded (Go: `ExportResponse.Bytes`).
    pub fn bytes(&self) -> Result<Vec<u8>> {
        base64_decode(self.content.as_deref().unwrap_or_default())
    }
}

impl WorkspaceApi {
    /// Import a file or notebook at `path` from `content`, as
    /// `multipart/form-data` (Go: `WorkspaceAPI.Upload`). A streamed
    /// `content` is read into memory first.
    pub async fn upload(
        &self,
        path: &str,
        content: impl Into<Binary>,
        options: UploadOptions,
    ) -> Result<()> {
        let mut options = options;
        if options
            .format
            .as_ref()
            .is_none_or(|f| *f == ImportFormat::Source)
            // Go infers even over an explicit language; here the caller's
            // choice wins.
            && options.language.is_none()
        {
            for (suffix, language) in [
                (".py", Language::Python),
                (".sql", Language::Sql),
                (".scala", Language::Scala),
                (".R", Language::R),
            ] {
                if path.ends_with(suffix) {
                    options.language = Some(language);
                }
            }
        }
        let content = content.into().bytes().await?;
        let boundary = idempotency_token();
        let mut form = Form::new(&boundary);
        form.field("path", path.as_bytes());
        form.file("content", &content);
        if let Some(f) = &options.format {
            form.field("format", f.to_string().as_bytes());
        }
        if let Some(l) = &options.language {
            form.field("language", l.to_string().as_bytes());
        }
        if options.overwrite {
            form.field("overwrite", b"true");
        }
        let call = Call::new(Method::POST, "/api/2.0/workspace/import".to_owned())
            .workspace()
            .body_with_type(
                Binary::from(form.finish()),
                format!("multipart/form-data; boundary={boundary}"),
            );
        self.api
            .send::<serde::de::IgnoredAny>(call)
            .await
            .map(|_| ())
    }

    /// Write `data` to the file `name`, detecting the format and replacing
    /// any existing file (Go: `WorkspaceAPI.WriteFile`).
    pub async fn write_file(&self, name: &str, data: impl Into<Binary>) -> Result<()> {
        self.upload(
            name,
            data,
            UploadOptions::default()
                .format(ImportFormat::Auto)
                .overwrite(),
        )
        .await
    }

    /// Export `path` directly, as a stream of its bytes (Go:
    /// `WorkspaceAPI.Download`).
    pub async fn download(&self, path: &str, options: DownloadOptions) -> Result<Binary> {
        let mut query = vec![
            ("path".to_owned(), path.to_owned()),
            ("direct_download".to_owned(), "true".to_owned()),
        ];
        if let Some(f) = options.format {
            query.push(("format".to_owned(), f.to_string()));
        }
        let call = Call::new(Method::GET, "/api/2.0/workspace/export".to_owned())
            .workspace()
            .query(query);
        Ok(self.api.send_binary(call).await?.0)
    }

    /// The contents of the file `name` (Go: `WorkspaceAPI.ReadFile`).
    pub async fn read_file(&self, name: &str) -> Result<Bytes> {
        self.download(name, DownloadOptions::default())
            .await?
            .bytes()
            .await
    }

    /// Every non-directory object under `path`, breadth first; folders
    /// that vanish while listing are skipped (Go:
    /// `WorkspaceAPI.RecursiveList`).
    pub async fn recursive_list(&self, path: &str) -> Result<Vec<ObjectInfo>> {
        let mut results = Vec::new();
        let mut queue = VecDeque::from([path.to_owned()]);
        while let Some(dir) = queue.pop_front() {
            let batch = match self.list_all(ListWorkspaceRequest::new(dir.clone())).await {
                Ok(b) => b,
                Err(e) if e.is_missing() => continue,
                Err(e) => return Err(Error::OperationFailed(format!("list {dir}: {e}"))),
            };
            for v in batch {
                if v.object_type == Some(ObjectType::Directory) {
                    queue.push_back(v.path.clone().unwrap_or_default());
                } else {
                    results.push(v);
                }
            }
        }
        Ok(results)
    }
}

/// A `multipart/form-data` body.
struct Form {
    boundary: String,
    body: Vec<u8>,
}

impl Form {
    fn new(boundary: &str) -> Self {
        Self {
            boundary: boundary.to_owned(),
            body: Vec::new(),
        }
    }

    fn part(&mut self, headers: &str, value: &[u8]) {
        self.body
            .extend_from_slice(format!("--{}\r\n{headers}\r\n\r\n", self.boundary).as_bytes());
        self.body.extend_from_slice(value);
        self.body.extend_from_slice(b"\r\n");
    }

    fn field(&mut self, name: &str, value: &[u8]) {
        self.part(
            &format!("Content-Disposition: form-data; name=\"{name}\""),
            value,
        );
    }

    fn file(&mut self, name: &str, value: &[u8]) {
        self.part(
            &format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream"
            ),
            value,
        );
    }

    fn finish(mut self) -> Vec<u8> {
        self.body
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        self.body
    }
}
