//! Núcleo do omarchy-drive: modelo de dados, trait de provedor, planejador de
//! sincronização de 3 vias, estado persistente e motor de execução.

pub mod config;
pub mod engine;
pub mod hash;
pub mod http;
pub mod ignore;
pub mod model;
pub mod oauth;
pub mod planner;
pub mod provider;
pub mod scan;
pub mod shortcut;
pub mod state;
pub mod status;
pub mod token_store;
pub mod trash;
pub mod tree;

pub use model::*;
pub use provider::{ChangeBatch, Provider, Quota};
