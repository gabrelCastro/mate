// Ajusta as opções de link para gerar um módulo que o Node consegue carregar
// (no macOS, por exemplo, os símbolos da N-API só existem dentro do Node).
fn main() {
    napi_build::setup();
}
