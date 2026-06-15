use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, GithubReleaseOptions,
    LanguageServerInstallationStatus, Os, Result,
};

struct CssLens {
    cached_binary_path: Option<String>,
}

impl zed::Extension for CssLens {
    fn new() -> Self {
        CssLens {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        // Attempt latest release first; fall back to a known tag if the
        // GitHub API returns 401 (a known Zed API bug for unauthenticated calls).
        // Note: github_release_by_tag_name does not accept GithubReleaseOptions.
        let release = zed::latest_github_release(
            "joshuaerney/css-lens",
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )
        .or_else(|_| {
            zed::github_release_by_tag_name("joshuaerney/css-lens", "v0.9.0")
        })
        .map_err(|e| {
            format!("Failed to fetch any GitHub release from css-lens (tried latest and v0.9.0): {e}")
        })?;

        let (platform, arch) = zed::current_platform();
        let target = match (platform, arch) {
            (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
            (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
            (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
            (Os::Windows, _) => return Err("Windows is not yet supported".to_string()),
            _ => {
                return Err(format!(
                    "Unsupported platform/architecture combination: {platform:?}/{arch:?}"
                ))
            }
        };

        let version = &release.version;
        let asset_name = format!("css-lens-{version}-{target}.tar.gz");

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| {
                let available: Vec<&String> = release.assets.iter().map(|a| &a.name).collect();
                format!(
                    "Asset '{asset_name}' not found in release {version}. \
                     Available assets: {available:?}"
                )
            })?;

        let binary_dir = format!("css-lens-{version}");
        let binary_path = format!("{binary_dir}/css-lens");

        // If the binary already exists on disk for this version, skip the download.
        // We intentionally do NOT require cached_binary_path to be set — that field
        // is reset when Zed restarts, so relying on it would cause a re-download on
        // every Zed launch even when the binary is already present.
        if std::path::Path::new(&binary_path).exists() {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::None,
            );
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(zed::Command {
                command: binary_path,
                args: vec![],
                env: vec![],
            });
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::Downloading,
        );

        zed::download_file(&asset.download_url, &binary_dir, DownloadedFileType::GzipTar)
            .map_err(|e| format!("Failed to download '{asset_name}': {e}"))?;

        zed::make_file_executable(&binary_path)
            .map_err(|e| format!("Failed to make '{binary_path}' executable: {e}"))?;

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::None,
        );

        self.cached_binary_path = Some(binary_path.clone());

        Ok(zed::Command {
            command: binary_path,
            args: vec![],
            env: vec![],
        })
    }

    // Annotates each completion with its source CSS filename: `btn — styles.css`
    fn label_for_completion(
        &self,
        _language_server_id: &zed::LanguageServerId,
        completion: zed::lsp::Completion,
    ) -> Option<zed::CodeLabel> {
        let detail = completion.detail.as_deref().unwrap_or("");
        let display = if detail.is_empty() {
            completion.label.clone()
        } else {
            format!("{} \u{2014} {}", completion.label, detail)
        };
        let filter_end = completion.label.len() as u32;
        Some(zed::CodeLabel {
            code: display.clone(),
            spans: vec![zed::CodeLabelSpan::literal(display, None)],
            filter_range: (0..filter_end).into(),
        })
    }
}

zed::register_extension!(CssLens);
