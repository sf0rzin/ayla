# Política de privacidade — Ayla Cookies for Microsoft Edge

Última atualização: 20 de agosto de 2026.

## Resumo

A extensão processa cookies e dados de LocalStorage somente no dispositivo do
usuário. Ela não possui servidor próprio, telemetria, publicidade, analytics ou
código remoto e não vende, compartilha nem transmite cookies, credenciais ou
histórico de navegação.

## Dados acessados

- Cookies dos sites permitidos pelo Microsoft Edge, apenas para listar, criar,
  editar, importar, exportar, proteger ou excluir conforme uma ação do usuário.
- A URL da aba atual, para mostrar e executar ações do site atual.
- LocalStorage de uma origem, somente quando o usuário solicita sua limpeza.

Arquivos `cookies.txt` e JSON são lidos localmente. Exportações são criadas
somente após uma ação explícita e permanecem sob o controle do usuário.

## Armazenamento e retenção

Preferências e identidades sem valor dos cookies protegidos podem permanecer no
`chrome.storage.local`. Valores usados para restaurar cookies protegidos ficam
no `chrome.storage.session`, separado entre os contextos normal e anônimo. A
ação **Apagar todos os cookies** desativa a restauração e limpa esses snapshots
antes de excluir os cookies do contexto atual.

As preferências são armazenadas em chaves separadas para os contextos normal e
anônimo, evitando que uma ação privada altere a proteção do perfil normal.

O usuário pode remover todos os dados da extensão a qualquer momento pela tela
de extensões do Edge, removendo ou redefinindo a extensão.

## Permissões

- `cookies`, `http://*/*` e `https://*/*`: necessários para gerenciar cookies de
  sites da Web. A extensão não solicita `<all_urls>` nem acesso por host a
  arquivos locais, FTP ou páginas internas do navegador.
- `storage`: guarda preferências e o estado local de proteção.
- `browsingData`: remove LocalStorage somente sob comando do usuário.
- `contextMenus`: oferece atalhos locais para abrir o gerenciador e limpar o
  site selecionado.

## Publicação

Antes de publicar na Microsoft Edge Add-ons, esta política deve ser hospedada em
uma URL pública estável e vinculada à ficha da extensão. Alterações materiais de
tratamento de dados devem atualizar este documento e a data acima.
