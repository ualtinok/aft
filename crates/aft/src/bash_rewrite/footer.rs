/// Add the educational bash-rewrite footer to a tool's human-readable output.
pub fn add_footer(response_output: &str, original_command: &str, replacement_tool: &str) -> String {
    format!(
        "{}\n\n(Note: rewritten from `{}` → call the `{}` tool directly next time to skip this rewrite.)",
        response_output, original_command, replacement_tool
    )
}
