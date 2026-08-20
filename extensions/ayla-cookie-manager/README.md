# Ayla Cookies for Microsoft Edge

Extensão Manifest V3 preparada para o Microsoft Edge 130 ou mais recente, com interface inspirada no Ayla e processamento inteiramente local. Continua compatível com navegadores baseados em Chromium 130+.

## Recursos

- Listar e pesquisar cookies por domínio, nome ou valor.
- Criar e editar nome, valor, domínio, caminho, expiração, SameSite, Secure e HttpOnly.
- Excluir cookies individualmente ou em massa, preservando os protegidos.
- Apagar todos os cookies acessíveis do perfil atual, incluindo os protegidos, com confirmação explícita e limpeza dos snapshots de proteção.
- Proteger cookies e cookies de sessão contra remoções observáveis pela API do navegador, com ações separadas para proteger e desproteger.
- Importar todos os registros válidos de JSON ou Netscape `cookies.txt`, inclusive linhas `#HttpOnly_`, com prévia de substituições, inválidos e duplicatas.
- Limpar o LocalStorage do site atual.
- Limpar cookies não protegidos na inicialização, quando ativado.
- Acessar stores normal/anônimo disponíveis e cookies particionados (CHIPS), incluindo a identidade `hasCrossSiteAncestor` exposta a partir do Chromium 130.

## Instalação local

1. Abra `edge://extensions` no Microsoft Edge.
2. Ative **Modo do desenvolvedor**.
3. Clique em **Carregar sem compactação**.
4. Selecione esta pasta (`extensions/ayla-cookie-manager`).

Para acessar cookies de janelas anônimas, habilite **Permitir no modo anônimo** nos detalhes da extensão.

## Importar cookies.txt

Abra o popup e clique em **Importar cookies.txt**, ou abra o gerenciador e use **Importar TXT**. A extensão aceita:

- Netscape `cookies.txt` com sete colunas separadas por tabulação;
- linhas `#HttpOnly_` usadas por exportadores compatíveis;
- backups JSON do Ayla e formatos JSON equivalentes com `domain`, `name` e `value`.

O arquivo é analisado localmente antes da gravação. Duplicatas usam a última ocorrência, linhas inválidas são contabilizadas sem expor seus valores e cookies existentes com a mesma identidade exigem confirmação antes de serem substituídos. Arquivos individuais podem ter até 32 MiB; divida arquivos maiores em partes para manter a interface responsiva.

O formato Netscape não possui campos para a chave de partição CHIPS. Por isso, uma exportação Netscape que inclua cookies particionados é recusada; use o backup JSON para preservar `partitionKey`, `topLevelSite` e `hasCrossSiteAncestor` sem perda.

## Apagar todos os cookies

O botão **Apagar todos os cookies do Edge** remove todos os cookies acessíveis no contexto atual da extensão, inclusive particionados e protegidos. A ação sempre pede confirmação, desativa a restauração automática e limpa os snapshots de proteção antes da remoção para que os cookies não reapareçam. O contexto normal e o anônimo permanecem separados pelo modelo `incognito: split` do Edge.

## Diferenças inevitáveis para o Cookie Quick Manager

O Chromium não oferece equivalentes para Firefox Multi-Account Containers e First-Party Isolation. A extensão usa os stores expostos pelo Chromium e oferece suporte a cookies particionados via `partitionKey`, inclusive `hasCrossSiteAncestor`; esse último campo determina o requisito mínimo Chromium 130. A proteção também depende dos eventos que o Chromium entrega à extensão; uma limpeza feita no encerramento do navegador pode ocorrer quando o service worker já não está disponível.

## Segurança

A permissão `cookies` e o acesso aos hosts `http://*/*` e `https://*/*` são necessários para gerenciar cookies da Web. A extensão não solicita `<all_urls>` e não obtém acesso por host a esquemas como `file:`, `ftp:` ou páginas internas do navegador. Nenhum cookie é enviado para servidores. Importações e exportações só acontecem por ação explícita do usuário e podem conter credenciais de sessão; trate esses arquivos como secretos.

Valores de cookies protegidos permanecem somente em `chrome.storage.session`, em chaves distintas para os contextos normal e anônimo, e o acesso às áreas de armazenamento é limitado a contextos confiáveis da extensão. O `storage.local` guarda apenas preferências e identidades sem valor dos cookies protegidos no contexto normal, para reidratação a partir do cookie jar no início de uma nova sessão. Registros legados que continham snapshots completos são purgados durante a migração. O snapshot anônimo é purgado ao fechar a última janela privada e nunca grava o valor do cookie em armazenamento persistente compartilhado.

As preferências também usam chaves independentes para os contextos normal e anônimo. Assim, apagar todos os cookies ou alterar a proteção em uma janela privada não muda silenciosamente a configuração do perfil normal.

Edições que mudam a identidade de um cookie recusam colisões existentes, gravam e validam o destino antes de remover a origem e tentam restaurar o estado anterior se qualquer etapa falhar. Mesmo assim, sites podem alterar cookies concorrentemente; quando a interface indicar uma reversão incompleta, atualize a lista antes de continuar.

A política de tratamento local está em [PRIVACY.md](PRIVACY.md). Para publicar
na Microsoft Edge Add-ons, hospede esse texto em uma URL pública estável e use a
mesma URL na ficha da extensão.

## Validação da extensão

Na raiz do repositório, execute a checagem estática e a suíte de regressão com mocks das APIs do Chromium:

```powershell
npm run check:extension
npm test --prefix extensions/ayla-cookie-manager
```
