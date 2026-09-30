//! Помощник установки обновления для deb и rpm (docs/PROMPT.md §3 «Обновления»).
//!
//! Окно запускает его через `pkexec /usr/libexec/melogold/melogold-update [файл]`; polkit-действие
//! `app.melogold.Melogold.update` в активном локальном сеансе не спрашивает пароль. Поэтому помощник
//! ничему от окна не доверяет, кроме пути к скачанному файлу, а тот — лишь подсказка, чтобы не
//! качать второй раз:
//!
//! - манифест читается с зашитого адреса GitHub, а не из аргументов и окружения;
//! - формат установленного пакета (`rpm -q` или `dpkg-query`) определяется здесь же;
//! - ставится только версия новее установленной, и только файл, чьи размер и SHA-256 совпали
//!   с манифестом; файл сначала копируется в каталог, доступный лишь root, и проверяется уже там;
//! - файл окна не подошёл или его нет — качаем сами, только с
//!   `https://github.com/melogold-app/melogoldLinux/releases/download/…`.
//!
//! Итог — строка в stdout (`ok <версия>` или `error <причина>: <подробности>`) и код выхода.
//!
//! Проверка без GitHub: `MELOGOLD_UPDATE_SOURCE=<папка>` с `update.json` и файлами вместо сети.
//! Переменная читается, только если процесс запущен root-ом напрямую: под `pkexec` всегда есть
//! `PKEXEC_UID`, и pkexec сам чистит окружение, так что пользователь через неё ничего не подложит.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use melogold_core::updates::{self, PackageFormat, UpdateAsset, UpdateManifest};

/// Каталог для проверенного файла: создаётся с правами 0700, принадлежит root.
const WORK_DIR: &str = "/var/lib/melogold-update";
const PACKAGE: &str = "melogold";
const SEARCH_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
const MAX_MANIFEST_BYTES: usize = 1 << 20;

/// Причина отказа: код выхода, короткое имя для окна и подробности для журнала.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    code: u8,
    name: &'static str,
    detail: String,
}

fn failure(code: u8, name: &'static str, detail: impl Into<String>) -> Failure {
    Failure { code, name, detail: detail.into() }
}

fn usage(detail: impl Into<String>) -> Failure {
    failure(2, "usage", detail)
}
fn not_newer(detail: impl Into<String>) -> Failure {
    failure(3, "not-newer", detail)
}
fn network(detail: impl Into<String>) -> Failure {
    failure(4, "network", detail)
}
fn unsupported(detail: impl Into<String>) -> Failure {
    failure(5, "unsupported", detail)
}
fn checksum(detail: impl Into<String>) -> Failure {
    failure(6, "checksum", detail)
}
fn install_failed(detail: impl Into<String>) -> Failure {
    failure(7, "install", detail)
}

/// Откуда брать манифест и файлы.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    Github,
    /// Только для проверки: папка с `update.json` и файлами.
    Directory(PathBuf),
}

/// Папка вместо GitHub — лишь у root, запущенного напрямую (без `PKEXEC_UID`).
fn choose_source(override_dir: Option<OsString>, pkexec_uid: Option<OsString>) -> Source {
    match override_dir.filter(|d| !d.is_empty()) {
        Some(dir) if pkexec_uid.is_none() => Source::Directory(PathBuf::from(dir)),
        _ => Source::Github,
    }
}

/// Что установлено: формат пакета и версия.
#[derive(Debug, PartialEq, Eq)]
struct Installed {
    format: PackageFormat,
    version: String,
}

fn parse_rpm(output: &str) -> Option<Installed> {
    let version = output.trim();
    (!version.is_empty() && !version.contains("not installed"))
        .then(|| Installed { format: PackageFormat::Rpm, version: version.to_owned() })
}

fn parse_dpkg(output: &str) -> Option<Installed> {
    let (status, version) = output.trim().split_once('|')?;
    (status == "install ok installed" && !version.is_empty()).then(|| Installed { format: PackageFormat::Deb, version: version.to_owned() })
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).env_clear().env("PATH", SEARCH_PATH).env("LANG", "C").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn query_installed() -> Option<Installed> {
    command_output("/usr/bin/rpm", &["-q", "--qf", "%{VERSION}", PACKAGE])
        .and_then(|out| parse_rpm(&out))
        .or_else(|| command_output("/usr/bin/dpkg-query", &["-W", "-f=${Status}|${Version}", PACKAGE]).and_then(|out| parse_dpkg(&out)))
}

/// Версия — только числа через точку: она попадает в адрес скачивания.
fn version_is_plain(version: &str) -> bool {
    !version.is_empty() && version.split('.').all(|p| !p.is_empty() && p.len() <= 6 && p.chars().all(|c| c.is_ascii_digit()))
}

/// Что ставить: файл манифеста для установленного формата, если он новее установленного.
fn plan<'a>(manifest: &'a UpdateManifest, installed: &Installed) -> Result<&'a UpdateAsset, Failure> {
    if !version_is_plain(&manifest.version) {
        return Err(unsupported(format!("странная версия в манифесте: {:?}", manifest.version)));
    }
    if !updates::is_newer(&manifest.version, &installed.version) {
        return Err(not_newer(format!("в манифесте {}, установлено {}", manifest.version, installed.version)));
    }
    let asset = manifest
        .package(installed.format)
        .ok_or_else(|| unsupported(format!("в релизе {} нет файла {} для этой архитектуры", manifest.version, installed.format.key())))?;
    let extension = match installed.format {
        PackageFormat::Deb => ".deb",
        PackageFormat::Rpm => ".rpm",
    };
    if !asset.has_safe_name() || !asset.file_name.ends_with(extension) {
        return Err(unsupported(format!("недопустимое имя файла: {:?}", asset.file_name)));
    }
    Ok(asset)
}

/// Адрес скачивания — только из релизов Melogold на GitHub.
fn official_url(manifest: &UpdateManifest, asset: &UpdateAsset) -> Result<String, Failure> {
    let url = manifest.download_url(asset);
    let prefix = format!("{}/v", updates::DOWNLOAD_BASE);
    if url.starts_with(&prefix) && version_is_plain(&manifest.version) && asset.has_safe_name() {
        Ok(url)
    } else {
        Err(unsupported(format!("чужой адрес: {url}")))
    }
}

/// Каталог для файла: создан root-ом с правами 0700 (или уже такой); прежнее содержимое убирается.
fn prepare_work_dir(dir: &Path) -> Result<(), Failure> {
    let fail = |e: std::io::Error| install_failed(format!("рабочий каталог {}: {e}", dir.display()));
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o077 != 0 {
                return Err(install_failed(format!("{} не принадлежит root или открыт другим", dir.display())));
            }
            for entry in fs::read_dir(dir).map_err(fail)?.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            }
            Ok(())
        }
        Err(_) => fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(fail),
    }
}

/// Файл, который положило окно: обычный файл (не ссылка, не канал) нужного размера — читаем
/// не больше, чем в манифесте. Не подошёл — `None`, и файл скачается заново.
fn read_given(path: &Path, asset: &UpdateAsset) -> Option<Vec<u8>> {
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() != asset.size_bytes {
        return None;
    }
    let mut data = Vec::with_capacity(asset.size_bytes as usize);
    file.take(asset.size_bytes + 1).read_to_end(&mut data).ok()?;
    asset.check(&data).ok().map(|()| data)
}

/// Данные — в рабочий каталог, но только если размер и SHA-256 совпали с манифестом.
fn stage(asset: &UpdateAsset, data: &[u8], work_dir: &Path) -> Result<PathBuf, Failure> {
    asset.check(data).map_err(|e| checksum(e.to_string()))?;
    prepare_work_dir(work_dir)?;
    let target = work_dir.join(&asset.file_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&target).map_err(|e| install_failed(format!("{}: {e}", target.display())))?;
    std::io::Write::write_all(&mut file, data).map_err(|e| install_failed(e.to_string()))?;
    Ok(target)
}

fn run_installer(mut command: Command, what: &str) -> Result<(), Failure> {
    let output = command
        .env_clear()
        .env("PATH", SEARCH_PATH)
        .env("HOME", "/root")
        .env("LANG", "C.UTF-8")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
        .map_err(|e| install_failed(format!("{what}: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let tail = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        text.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ")
    };
    Err(install_failed(format!("{what}: {}; {} / {}", output.status, tail(&output.stderr), tail(&output.stdout))))
}

fn install(format: PackageFormat, file: &Path) -> Result<(), Failure> {
    match format {
        PackageFormat::Rpm => {
            if Path::new("/usr/bin/dnf").exists() {
                let mut command = Command::new("/usr/bin/dnf");
                command.args(["install", "-y"]).arg(file);
                run_installer(command, "dnf install")
            } else {
                let mut command = Command::new("/usr/bin/rpm");
                command.args(["-U", "--quiet"]).arg(file);
                run_installer(command, "rpm -U")
            }
        }
        PackageFormat::Deb => {
            let mut command = Command::new("/usr/bin/apt-get");
            command.args(["install", "-y", "-o", "APT::Sandbox::User=root", "-o", "Dpkg::Options::=--force-confold"]).arg(file);
            run_installer(command, "apt-get install")
        }
    }
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Failure> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| network(e.to_string()))? {
        data.extend_from_slice(&chunk);
        if data.len() > limit {
            return Err(network("ответ больше, чем в манифесте"));
        }
    }
    Ok(data)
}

fn http_client() -> Result<reqwest::Client, Failure> {
    reqwest::Client::builder()
        .user_agent(melogold_core::app_info::tool_user_agent())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .https_only(true)
        .build()
        .map_err(|e| network(e.to_string()))
}

async fn fetch_manifest(source: &Source) -> Result<UpdateManifest, Failure> {
    let text = match source {
        Source::Github => {
            let response = http_client()?
                .get(updates::MANIFEST_URL)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| network(e.to_string()))?;
            String::from_utf8_lossy(&read_limited(response, MAX_MANIFEST_BYTES).await?).into_owned()
        }
        Source::Directory(dir) => fs::read_to_string(dir.join("update.json")).map_err(|e| network(e.to_string()))?,
    };
    serde_json::from_str(&text).map_err(|e| network(format!("манифест не разобрался: {e}")))
}

async fn fetch_file(source: &Source, manifest: &UpdateManifest, asset: &UpdateAsset) -> Result<Vec<u8>, Failure> {
    match source {
        Source::Github => {
            let url = official_url(manifest, asset)?;
            let response =
                http_client()?.get(&url).send().await.and_then(reqwest::Response::error_for_status).map_err(|e| network(e.to_string()))?;
            read_limited(response, asset.size_bytes as usize).await
        }
        Source::Directory(dir) => fs::read(dir.join(&asset.file_name)).map_err(|e| network(e.to_string())),
    }
}

async fn run(given: Option<PathBuf>) -> Result<String, Failure> {
    // SAFETY: geteuid не читает и не пишет память.
    if unsafe { libc::geteuid() } != 0 {
        return Err(usage("нужны права root: запускайте через pkexec"));
    }
    let source = choose_source(std::env::var_os("MELOGOLD_UPDATE_SOURCE"), std::env::var_os("PKEXEC_UID"));
    let installed = query_installed().ok_or_else(|| unsupported("Melogold не установлен пакетом deb или rpm"))?;
    let manifest = fetch_manifest(&source).await?;
    let asset = plan(&manifest, &installed)?.clone();
    let work_dir = Path::new(WORK_DIR);
    // Файл окна — только подсказка, чтобы не качать второй раз; не подошёл — качаем сами.
    let data = match given.as_deref().and_then(|path| read_given(path, &asset)) {
        Some(data) => data,
        None => fetch_file(&source, &manifest, &asset).await?,
    };
    let file = stage(&asset, &data, work_dir)?;
    let result = install(installed.format, &file);
    let _ = fs::remove_dir_all(work_dir);
    result?;
    match query_installed() {
        Some(now) if now.version == manifest.version => Ok(manifest.version),
        other => Err(install_failed(format!("после установки версия {other:?}, ждали {}", manifest.version))),
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let given = args.next().map(PathBuf::from);
    let outcome = if args.next().is_some() {
        Err(usage("ожидается один аргумент — путь к скачанному файлу"))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| network(e.to_string()))
            .and_then(|rt| rt.block_on(run(given)))
    };
    match outcome {
        Ok(version) => println!("ok {version}"),
        Err(f) => {
            println!("error {}: {}", f.name, f.detail.replace('\n', " "));
            std::process::exit(i32::from(f.code));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sha2::{Digest, Sha256};

    use super::*;

    fn sha(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    fn manifest(version: &str, file: &str, data: &[u8]) -> UpdateManifest {
        let asset = UpdateAsset { file_name: file.into(), size_bytes: data.len() as u64, sha256: sha(data) };
        let per_arch = HashMap::from([(std::env::consts::ARCH.to_owned(), asset)]);
        UpdateManifest {
            version: version.into(),
            packages: HashMap::from([("rpm".to_owned(), per_arch.clone()), ("deb".to_owned(), per_arch)]),
            ..Default::default()
        }
    }

    fn installed(format: PackageFormat, version: &str) -> Installed {
        Installed { format, version: version.into() }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melogold-update-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn override_dir_only_without_pkexec() {
        let dir = || Some(OsString::from("/tmp/x"));
        assert_eq!(choose_source(dir(), None), Source::Directory(PathBuf::from("/tmp/x")));
        assert_eq!(choose_source(dir(), Some("1000".into())), Source::Github);
        assert_eq!(choose_source(None, None), Source::Github);
        assert_eq!(choose_source(Some("".into()), None), Source::Github);
    }

    #[test]
    fn manifest_url_is_the_official_one() {
        assert_eq!(updates::MANIFEST_URL, "https://github.com/melogold-app/melogoldLinux/releases/latest/download/update.json");
        assert_eq!(updates::DOWNLOAD_BASE, "https://github.com/melogold-app/melogoldLinux/releases/download");
    }

    #[test]
    fn parses_installed_versions() {
        assert_eq!(parse_rpm("0.1.4\n"), Some(installed(PackageFormat::Rpm, "0.1.4")));
        assert_eq!(parse_rpm("package melogold is not installed"), None);
        assert_eq!(parse_dpkg("install ok installed|0.1.4"), Some(installed(PackageFormat::Deb, "0.1.4")));
        assert_eq!(parse_dpkg("deinstall ok config-files|0.1.4"), None);
        assert_eq!(parse_dpkg(""), None);
    }

    #[test]
    fn plan_picks_the_file_of_the_installed_format() {
        let m = manifest("0.1.5", "melogold_0.1.5_amd64.deb", b"deb");
        if std::env::consts::ARCH == "x86_64" {
            let asset = plan(&m, &installed(PackageFormat::Deb, "0.1.4")).unwrap();
            assert_eq!(asset.file_name, "melogold_0.1.5_amd64.deb");
        }
        // Имя .deb у rpm-записи — не наш файл.
        assert_eq!(plan(&m, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().name, "unsupported");
    }

    #[test]
    fn plan_refuses_same_and_older_versions() {
        let same = manifest("0.1.4", "melogold-0.1.4-1.fc43.x86_64.rpm", b"x");
        let older = manifest("0.1.3", "melogold-0.1.3-1.fc43.x86_64.rpm", b"x");
        assert_eq!(plan(&same, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().code, 3);
        assert_eq!(plan(&older, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().code, 3);
    }

    #[test]
    fn plan_refuses_missing_and_strange() {
        let empty = UpdateManifest { version: "0.1.5".into(), ..Default::default() };
        assert_eq!(plan(&empty, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().code, 5);
        let evil = manifest("0.1.5", "../evil.rpm", b"x");
        assert_eq!(plan(&evil, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().code, 5);
        let odd = manifest("0.1.5/../../x", "melogold-0.1.5-1.x86_64.rpm", b"x");
        assert_eq!(plan(&odd, &installed(PackageFormat::Rpm, "0.1.4")).unwrap_err().code, 5);
    }

    #[test]
    fn download_url_is_only_from_our_releases() {
        let good = manifest("0.1.5", "melogold-0.1.5-1.x86_64.rpm", b"x");
        let asset = good.packages["rpm"].values().next().unwrap();
        assert_eq!(
            official_url(&good, asset).unwrap(),
            "https://github.com/melogold-app/melogoldLinux/releases/download/v0.1.5/melogold-0.1.5-1.x86_64.rpm"
        );
        let mut bad = manifest("0.1.5", "a.rpm", b"x");
        bad.version = "0.1.5@evil.example".into();
        let asset = bad.packages["rpm"].values().next().unwrap();
        assert_eq!(official_url(&bad, asset).unwrap_err().code, 5);
        let mut slash = manifest("0.1.5", "a.rpm", b"x");
        slash.packages.get_mut("rpm").unwrap().values_mut().for_each(|a| a.file_name = "../../../evil/a.rpm".into());
        let asset = slash.packages["rpm"].values().next().unwrap();
        assert_eq!(official_url(&slash, asset).unwrap_err().code, 5);
    }

    #[test]
    fn good_file_is_staged_in_the_work_dir() {
        let dir = scratch("staged");
        let data = b"package bytes";
        let m = manifest("0.1.5", "melogold-0.1.5-1.x86_64.rpm", data);
        let asset = m.packages["rpm"].values().next().unwrap();
        let work = dir.join("work");
        let file = stage(asset, data, &work).unwrap();
        assert_eq!(fs::read(&file).unwrap(), data);
        assert_eq!(file.parent().unwrap(), work);
        assert_eq!(fs::metadata(&work).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn wrong_hash_or_size_installs_nothing() {
        let dir = scratch("nothing");
        let m = manifest("0.1.5", "melogold-0.1.5-1.x86_64.rpm", b"package bytes");
        let asset = m.packages["rpm"].values().next().unwrap();
        let work = dir.join("work");
        let error = stage(asset, b"package bytez", &work).unwrap_err();
        assert_eq!((error.code, error.name), (6, "checksum"));
        assert_eq!(stage(asset, b"short", &work).unwrap_err().code, 6);
        assert!(!work.exists() || fs::read_dir(&work).unwrap().count() == 0, "в каталоге не должно остаться файла");
    }

    #[test]
    fn window_file_with_wrong_hash_is_ignored() {
        let dir = scratch("given-bad");
        let data = b"package bytes";
        let given = dir.join("from-window.rpm");
        fs::write(&given, b"package bytez").unwrap();
        let m = manifest("0.1.5", "melogold-0.1.5-1.x86_64.rpm", data);
        let asset = m.packages["rpm"].values().next().unwrap();
        assert!(read_given(&given, asset).is_none());
        fs::write(&given, data).unwrap();
        assert_eq!(read_given(&given, asset).as_deref(), Some(&data[..]));
    }

    #[test]
    fn symlink_and_wrong_size_from_window_are_ignored() {
        let dir = scratch("given-link");
        let data = b"package bytes";
        let real = dir.join("real.rpm");
        fs::write(&real, data).unwrap();
        let link = dir.join("link.rpm");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let asset = UpdateAsset { file_name: "a.rpm".into(), size_bytes: data.len() as u64, sha256: sha(data) };
        assert!(read_given(&link, &asset).is_none());
        assert!(read_given(&real, &asset).is_some());
        let bigger = UpdateAsset { size_bytes: 3, ..asset };
        assert!(read_given(&real, &bigger).is_none());
    }
}
