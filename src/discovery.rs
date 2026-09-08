use std::{
    io::Result,
    path::{Path, PathBuf},
};

const APPLICATION_NAME: &str = "nypa_db_server";

/// Find active publisher sockets in the process runtime directory, sorted by stream id.
pub fn find_publisher_sockets() -> Result<Vec<PathBuf>> {
    let mut sockets = Vec::new();

    for entry in std::fs::read_dir(publisher_socket_dir())? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(stream_id) = entry
            .file_name()
            .to_str()
            .and_then(extract_socket_name_info)
        else {
            continue;
        };

        sockets.push((entry.path(), stream_id));
    }

    sockets.sort_by_key(|(_, stream_id)| *stream_id);
    Ok(sockets.into_iter().map(|(path, _)| path).collect())
}

pub(crate) fn stream_id_from_socket_path(path: &Path) -> Option<usize> {
    let file_name = path.file_name()?.to_str()?;
    let name = file_name.strip_suffix(".sock")?;
    let (_, id) = name.rsplit_once('_')?;
    id.parse().ok()
}

fn publisher_socket_dir() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir)
}

fn extract_socket_name_info(file_name: &str) -> Option<usize> {
    let file_name = file_name.strip_suffix(".sock")?;
    let (context, ident) = file_name.rsplit_once('_')?;
    (context == APPLICATION_NAME).then(|| ident.parse().ok())?
}
