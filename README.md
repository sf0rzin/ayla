# Ayla

Aplicativo desktop em Rust, Tauri 2 e React/TypeScript. O projeto está sendo reconstruído de forma incremental, mantendo o núcleo Rust separado da interface para permitir a substituição completa do design.

## Estado atual

- Estrutura desktop Tauri 2.
- Catálogo dos 13 módulos originais.
- Configurações locais validadas e persistidas pelo backend.
- Parser de proxies em Rust com deduplicação e suporte a HTTP(S), SOCKS4 e SOCKS5.
- Gerenciador de proxies com importação, persistência, remoção e limpeza.
- Verificação concorrente com progresso em tempo real, timeout e cancelamento.
- Motor de tarefas em Rust executado em background, com progresso agregado, cancelamento seletivo e histórico seguro.
- MVP ChatGPT: filtro local de artefatos, validação autenticada de sessão/plano e proxy opcional.
- Página `Tarefas` para preparar execuções, acompanhar o trabalho ativo e consultar resumos anteriores.
- Interface desktop baseada no Grafite DS fornecido pelo usuário.
- Barra de janela própria, navegação em três painéis, Geist e ícones Lucide locais.
- Testes unitários e servidores locais simulados para HTTP, SOCKS4a e SOCKS5.

Os proxies que não respondem são removidos depois da verificação, seguindo o comportamento do projeto de referência. A atualização de cada proxy agora é O(1), com persistência única ao final da rodada, distribuição de trabalho por cursor atômico, canal limitado e timeout total por proxy.

O motor aceita uma execução global por vez nesta etapa. Antes de iniciar, remove vazios e duplicados em O(n) preservando a ordem e limita a execução a 10.000 arquivos únicos, 20.000 linhas brutas, 32 KiB por linha, 32 MiB de caminhos, 512 MiB de artefatos e 32 workers. Caminhos existem apenas no payload IPC e na memória transitória da execução: nunca entram em eventos, logs ou histórico e não são persistidos.

O adaptador ChatGPT lê no máximo 2 MiB por arquivo, valida domínio, expiração, valores, fan-out JSON e chunks e então confirma a sessão e o plano nos endpoints autenticados. O cliente preserva timeout, tentativas, concorrência e proxies HTTP/SOCKS ativos. Caminhos, cookies e tokens não entram em eventos, logs ou histórico; a interface recebe somente contagens agregadas.

## Desenvolvimento

Pré-requisitos: Node.js, Rust estável com alvo MSVC, Visual Studio C++ Build Tools e WebView2.

```powershell
npm install
npm run tauri dev
```

Validação completa:

```powershell
npm run check
```

## Estrutura

```text
src/                    interface desktop em React
src-tauri/src/catalog.rs catálogo de módulos
src-tauri/src/auth_artifact.rs parser local e classificação estrutural ChatGPT
src-tauri/src/proxy.rs   parser e normalização de proxies
src-tauri/src/proxy_store.rs persistência e operações da lista
src-tauri/src/proxy_checker.rs verificação concorrente e protocolos
src-tauri/src/task_engine.rs motor de tarefas, progresso, cancelamento e histórico
src-tauri/src/settings.rs configurações e persistência
src-tauri/src/lib.rs     comandos expostos à interface
```

## Interface

O visual usa como base o pacote `# Ayla.zip`: superfícies grafite em camadas, acento índigo, controles com raios generosos e ação primária clara. Os componentes foram reescritos em React e ligados ao backend existente; o protótipo HTML não é necessário durante a execução.

## Segurança

Cookies, sessões, licenças, proxies reais, resultados e pastas `tdata` não devem ser adicionados ao repositório. A suíte normal usa somente dados sintéticos. Exemplos externos permanecem fora do projeto: o app lê apenas caminhos fornecidos explicitamente e o teste ignorado exige opt-in; ambos retornam somente totais agregados. O histórico de tarefas armazena exclusivamente resumos: nenhuma entrada, caminho ou credencial é gravada.

## Próximas etapas

1. Migrar os próximos módulos individualmente, mantendo cada integração isolada e testada.
2. Autenticação/licenciamento com armazenamento seguro.
3. Evoluir o agendamento para múltiplas execuções globais, quando necessário.
4. Migração isolada do suporte a Telegram `tdata`.
