use lsp_types::ClientCapabilities;

use crate::commands::server::ServerCommand;

#[derive(Default)]
pub(crate) struct ServerConfig {
    pub(crate) args: ServerCommand,
    pub(crate) caps: ClientCapabilities,
}

macro_rules! try_or_default {
    ($expr:expr) => {
        (|| $expr)().unwrap_or_default()
    };
}

impl ServerConfig {
    pub(crate) fn has_text_document_definition_link_support(&self) -> bool {
        try_or_default!(self.caps.text_document.as_ref()?.definition?.link_support)
    }

    pub(crate) fn has_insert_replace_support(&self) -> bool {
        try_or_default!(
            self.caps
                .text_document
                .as_ref()?
                .completion
                .as_ref()?
                .completion_item
                .as_ref()?
                .insert_replace_support
        )
    }
}
