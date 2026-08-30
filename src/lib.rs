use zed_extension_api::{self as zed, LanguageServerId, Result};

struct Nari {

}

impl zed::Extension for Nari {
    fn new() -> Self {
        Self {}
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        Ok(zed::Command {
            command: "nari-lsp".to_string(),
            args: vec![],                       
            env: vec![],
        })
    }
}

zed::register_extension!(Nari);
