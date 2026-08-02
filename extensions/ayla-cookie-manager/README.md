# Ayla Cookie Manager

Extensão Manifest V3 para navegadores Chromium, com interface inspirada no Ayla e processamento inteiramente local.

## Recursos

- Listar e pesquisar cookies por domínio, nome ou valor.
- Criar e editar nome, valor, domínio, caminho, expiração, SameSite, Secure e HttpOnly.
- Excluir cookies individualmente ou em massa, preservando os protegidos.
- Proteger cookies e cookies de sessão contra remoções observáveis pela API do navegador.
- Importar e exportar JSON ou Netscape `cookies.txt`.
- Limpar o LocalStorage do site atual.
- Limpar cookies não protegidos na inicialização, quando ativado.
- Acessar stores normal/anônimo disponíveis e cookies particionados (CHIPS).

## Instalação local

1. Abra `chrome://extensions` no Chrome, Edge, Brave ou outro Chromium.
2. Ative **Modo do desenvolvedor**.
3. Clique em **Carregar sem compactação**.
4. Selecione esta pasta (`extensions/ayla-cookie-manager`).

Para acessar cookies de janelas anônimas, habilite **Permitir no modo anônimo** nos detalhes da extensão.

## Diferenças inevitáveis para o Cookie Quick Manager

O Chromium não oferece equivalentes para Firefox Multi-Account Containers e First-Party Isolation. A extensão usa os stores expostos pelo Chromium e oferece suporte a cookies particionados via `partitionKey`. A proteção também depende dos eventos que o Chromium entrega à extensão; uma limpeza feita no encerramento do navegador pode ocorrer quando o service worker já não está disponível.

## Segurança

A permissão `cookies` e o acesso a todos os hosts são necessários para um gerenciador global. Nenhum cookie é enviado para servidores. Exportações só são criadas por ação explícita do usuário e podem conter credenciais de sessão; trate esses arquivos como secretos.
