pub(crate) fn unauthenticated_repository_writer(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, "unguarded repository bytes")
}
