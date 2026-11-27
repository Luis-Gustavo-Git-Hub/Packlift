# Instruções para agentes

## Testes automatizados são obrigatórios

- Para cada correção, melhoria ou alteração de comportamento, crie ou atualize testes automatizados que cubram o comportamento alterado.
- Para correções de bugs, inclua um teste de regressão que exercite o cenário que falhava. Quando possível, confirme que o teste falha antes da correção e passa depois.
- Mudanças de interface também devem ter testes de comportamento ou da estrutura dos widgets. Se precisarem de uma tela gráfica, execute-os no CI com Xvfb; não os remova só porque o ambiente local não tem display.
- Não encerre uma tarefa com testes falhando. Corrija a causa e rode novamente os testes afetados.
- Antes de concluir uma alteração de código, rode:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets --all-features -- -D warnings
  xvfb-run -a make test
  cargo test --locked --no-default-features
  ```

- Para testes que criam widgets GTK, use `xvfb-run -a` no CI. Se uma validação não puder ser executada localmente, registre o motivo e confirme que o CI a executa antes de integrar a mudança.
- Mantenha os testes no mesmo commit ou PR da alteração que eles protegem e inclua os resultados das validações no resumo final.
