//! # Planner: da AST para um plano executável
//!
//! 1. [`plan`] — o [`Plan`], a árvore de operadores (e o texto do EXPLAIN).
//! 2. [`planner`] — validação, binding de nomes, agregação e escolha de
//!    índice.

pub mod plan;
#[allow(clippy::module_inception)]
pub mod planner;

pub use plan::Plan;
pub use planner::{QueryPlan, plan_statement};
