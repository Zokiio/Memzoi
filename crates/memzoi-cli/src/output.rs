use anyhow::Result;

pub(crate) fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub(crate) fn print_jsonl_row<T: ?Sized + serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}
