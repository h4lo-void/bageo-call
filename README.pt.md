<p align="center">
  <img src="docs/images/banner.en.svg" width="900" alt="Bageo: seu número, atendido por IA. Atende ligações, enrola golpistas e faz ligações por você.">
</p>

<p align="center">
  <a href="README.md">English</a> · <a href="README.zh-TW.md">繁體中文</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ko.md">한국어</a> · <a href="README.es.md">Español</a> · <a href="README.fr.md">Français</a> · <a href="README.de.md">Deutsch</a> · <strong>Português</strong>
</p>

<p align="center">
  <strong>Assistente de telefone com IA para o seu número de verdade: o Gemini Live atende e faz as suas ligações.</strong><br>
  Hospedado por você e de código aberto. Um celular Android com root que estava sobrando, com o seu SIM, um programa no seu computador e a sua própria chave do Gemini.
</p>

<p align="center">
  <a href="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml"><img src="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/h4lo-void/bageo-call/releases/tag/v1.6.9"><img src="https://img.shields.io/badge/release-v1.6.9-brightgreen.svg" alt="Versão v1.6.9"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg" alt="Licença: AGPL-3.0-only"></a>
  <a href="COMMERCIAL_LICENSE.md"><img src="https://img.shields.io/badge/commercial-dual%20license-gold.svg" alt="Licença comercial disponível"></a>
</p>

<table>
  <tr>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/answer.svg" width="56" height="56" alt=""><br>
      <strong>Atende suas ligações</strong><br>
      Atende depois de um toque, fala como uma pessoa e espera quem ligou se despedir.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/stall.svg" width="56" height="56" alt=""><br>
      <strong>Enrola os golpistas</strong><br>
      Percebe ligações de vendas e de golpe e vira um cliente lento e curioso por até 30 minutos.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/errand.svg" width="56" height="56" alt=""><br>
      <strong>Faz ligações por você</strong><br>
      Escreva uma linha e ele liga por você. Na dúvida, pergunta antes.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/languages.svg" width="56" height="56" alt=""><br>
      <strong>Fala nove idiomas</strong><br>
      Inglês, chinês tradicional, chinês simplificado, japonês, coreano, espanhol, francês, alemão e português. Troca de idioma quando quem liga troca.
    </td>
  </tr>
</table>

<p align="center"><sub>O nome vem de bāgē (八哥), o mainá-de-crista, uma ave que aprende a falar como as pessoas com quem convive.</sub></p>

## Teste: adivinhe o que o Bageo diz

Abra uma ligação e avance linha por linha. <sub>Exemplos de roteiro, não gravações de ligações reais.</sub>

<details>
<summary><strong>Entrega de pacote</strong> — Chega um pacote e você está ocupado.</summary>
<br>

> <strong>Quem liga</strong>: Seu pacote chegou.

<details>
<summary>O que o Bageo diz?</summary>
<br>

> <strong>Bageo</strong>: Olá, sou o assistente de IA que está atendendo este telefone. Precisa assinar pessoalmente?
>
> <strong>Quem liga</strong>: Sim. Podemos entregar de novo hoje, antes das cinco da tarde.

<details>
<summary>E depois?</summary>
<br>

> <strong>Bageo</strong>: Vou repassar e pedir que o destinatário confirme um horário.

<strong>O que você recebe:</strong> Precisa de assinatura; dá para entregar de novo antes das 17h. Você confirma o horário.

</details>
</details>
</details>

<details>
<summary><strong>Ligação de vendas</strong> — Uma “oferta especial” está ligando.</summary>
<br>

> <strong>Quem liga</strong>: Você foi aprovado hoje para um empréstimo com juros baixos!

<details>
<summary>O Bageo desliga?</summary>
<br>

> <strong>Bageo</strong>: Esses juros são ao mês ou ao ano?
>
> <strong>Quem liga</strong>: Primeiro me passe seus dados pessoais.

<details>
<summary>O Bageo passa os dados?</summary>
<br>

> <strong>Bageo</strong>: Como as taxas são calculadas? De quanto é a parcela mensal?

<strong>O que você recebe:</strong> Nenhum dado pessoal é passado e as perguntas não param. O Bageo consegue manter alguém assim ocupado por até 30 minutos.

</details>
</details>
</details>

<details>
<summary><strong>Reservar uma mesa</strong> — Uma instrução. Uma ligação a menos.</summary>
<br>

> <strong>Você</strong>: Reserve para quatro amanhã às 19h. Às 19h30 também serve.

<details>
<summary>Como o Bageo começa?</summary>
<br>

> <strong>Bageo</strong>: Olá, sou um assistente de IA e estou ligando para reservar uma mesa para quatro amanhã às sete.
>
> <strong>Restaurante</strong>: Às sete está lotado. Temos mesa às sete e meia.

<details>
<summary>O Bageo aceita?</summary>
<br>

> <strong>Bageo</strong>: Sete e meia serve. Pode reservar para quatro, obrigado.
>
> <strong>Restaurante</strong>: Confirmado: amanhã às sete e meia, quatro pessoas.

<strong>O que você recebe:</strong> Amanhã, 19h30, quatro pessoas, dentro do que você permitiu. Se algo sair das suas instruções, o Bageo pergunta a você antes.

</details>
</details>
</details>

## Um golpista liga. O Bageo o mantém falando.

<p align="center">
  <img src="docs/images/scene-stall.en.svg" width="760" alt="Uma ligação de venda de empréstimo. Quem liga diz que você foi pré-aprovado para um empréstimo com juros baixos; a IA pergunta se os juros são ao mês ou ao ano. Quem liga pede um número de documento; a IA faz mais perguntas em vez de responder. 27 minutos depois, quem liga quer desligar e a IA diz: espere, tenho mais uma pergunta. Resultado: 27 minutos enrolando, nenhum dado pessoal entregue, gravação e resumo salvos.">
</p>

Quando uma ligação é claramente de vendas ou de golpe, a IA vira um cliente interessado, lento e fácil de distrair. Pergunta cada detalhe, entende os números errado e nunca passa um número verdadeiro, por até 30 minutos, e você recebe a gravação, a transcrição e um resumo curto. Ligações honestas ficam em paz: um banco conferindo uma compra no cartão, uma clínica, um entregador, a família. “Manter a pessoa falando” vem ligado por padrão e um único botão desliga. [Como funciona a triagem](docs/en/FEATURES.md#spam-and-scam-call-blocker-with-stall-mode)

## Sem vontade de ligar? Escreva uma linha.

<p align="center">
  <img src="docs/images/scene-errand.en.svg" width="760" alt="Uma ligação da IA a seu pedido. Você escreveu: reserve uma mesa para quatro amanhã às 19h, às 19h30 também serve. A IA liga para o restaurante como assistente de Alex; às sete está lotado, o restaurante oferece sete e meia e a IA aceita. Resultado: feito, amanhã 19h30, quatro pessoas, pronto para a sua agenda.">
</p>

Reservar uma mesa, remarcar um horário, perguntar a uma loja sobre estoque ou horário de funcionamento. Quando o outro lado traz algo que você não previu, a IA pergunta a você na tela antes de responder. Ela nunca concorda com nada por conta própria: se você não responder a tempo, diz que você vai confirmar e encerra a ligação com educação. [Mais sobre as ligações da IA](docs/en/FEATURES.md#ai-errands-it-makes-calls-for-you)

## Ligação do exterior? O Bageo fala nove idiomas.

<p align="center">
  <img src="docs/images/languages.en.svg" width="900" alt="O Bageo fala nove idiomas: inglês, chinês tradicional, chinês simplificado, japonês, coreano, espanhol, francês, alemão e português. Cada um atende com o seu próprio olá.">
</p>

A voz da IA, as instruções embutidas (uma base curta e geral que você pode adaptar à sua situação e ao seu estilo), o console web, o app do celular e as notificações existem em nove idiomas. Em um celular com dois SIMs, cada SIM abre as ligações no seu próprio idioma; quem liga do exterior ouve o idioma do seu país e, se a pessoa trocar de idioma no meio da ligação, a IA acompanha. As transcrições guardam as palavras originais, cada linha com uma tradução para o idioma das suas notificações. [Mais sobre idiomas e dois SIMs](docs/en/FEATURES.md#nine-languages-dual-sim-translated-transcripts)

## Como funciona

<p align="center">
  <img src="docs/images/how-it-works.en.svg" width="900" alt="Como o Bageo funciona: quem liga chega ao seu celular reserva com root pela rede móvel; o celular passa o áudio da ligação por USB ou Wi-Fi ao programa Bageo no seu computador, que conversa com o Gemini Live. As gravações ficam no seu disco, as notificações vão para 32 canais e você controla tudo no console web.">
</p>

O celular só leva a ligação; todo o resto roda no seu próprio computador. A IA se conecta enquanto o celular ainda está tocando, então a primeira palavra dela sai cerca de **0.43 s** depois do atendimento (mediana medida). Uma ligação, passo a passo: [Arquitetura](docs/en/ARCHITECTURE.md).

## Por que o Bageo

| | Serviços de telefone com IA em troncos SIP ou números virtuais | Bageo |
|---|---|---|
| **Número** | Um número do serviço, ou as suas ligações redirecionadas para ele | O número que você já tem. As ligações que você faz também mostram esse número. |
| **Custo** | Uma assinatura ou um valor por minuto | Sem taxa de plataforma. Você paga ao Google pelo uso do Gemini e à operadora pelo SIM. |
| **Seus dados** | Nos servidores do serviço | Arquivos comuns em uma pasta `data/` no seu próprio disco |
| **O porém** | Cadastrar e usar | Você precisa de um celular Android com root sobrando e de um computador que fique ligado |

## E tem mais

<table>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/calendar.svg" width="56" height="56" alt=""><br>
      <strong>Os compromissos esperam por você</strong><br>
      Com “Compromissos” ligado, quem quer marcar um horário cai na sua lista de “Pendências”. Você aceita ou recusa; a IA avisa por SMS ou liga de volta.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/contacts.svg" width="56" height="56" alt=""><br>
      <strong>Sabe quem está ligando</strong><br>
      Importe contatos de vCard, Excel, PDF ou de uma foto de cartão de visita. Coloque cada pessoa em uma de quatro listas: contato, lista branca, lista de bloqueio ou lista para manter falando.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/switch.svg" width="56" height="56" alt=""><br>
      <strong>Um botão para voltar a ser um telefone normal</strong><br>
      Transforme o celular reserva em um telefone comum e volte, pelo console ou pelo app.
    </td>
  </tr>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/sound.svg" width="56" height="56" alt=""><br>
      <strong>Soa como uma ligação de verdade</strong><br>
      Som de fundo opcional: casa, escritório, rua, feira noturna e mais oito.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/notify.svg" width="56" height="56" alt=""><br>
      <strong>32 canais de notificação</strong><br>
      Um resumo depois de cada ligação no Telegram, Discord, Slack, LINE e mais. Resolva com um toque na notificação ou mande comandos pelo chat.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/local.svg" width="56" height="56" alt=""><br>
      <strong>Seus dados continuam seus</strong><br>
      As gravações e as transcrições são arquivos comuns no seu próprio disco. Sem conta conosco.
    </td>
  </tr>
</table>

Todos os recursos em detalhes: [Recursos](docs/en/FEATURES.md).

## O que você precisa

1. **Um celular Android reserva** com o seu SIM, com root pelo Magisk ou KernelSU, Android 10 ou mais novo. Testado: Google Pixel 7, 7a, 8 e 9.
2. **Um computador** ligado enquanto as ligações devem ser atendidas: Windows 10/11, macOS, Linux ou Docker.
3. **Um cabo USB** (Wi-Fi também funciona).
4. **Uma chave de API do Gemini** do [Google AI Studio](https://aistudio.google.com/apikey). O Google cobra por uso.

## Início rápido

Se o celular já tem root, são cerca de 3 minutos do download até a primeira chamada atendida pela IA.

1. **Baixe** o arquivo para o seu sistema em [Releases](https://github.com/h4lo-void/bageo-call/releases): `bageo-call-<version>-windows-x64-setup.exe` no Windows, `bageo-call-<version>-macos-arm64.dmg` (chip Apple) ou `bageo-call-<version>-macos-x64.dmg` (Intel) no macOS, `bageo-call-<version>-linux-x64.tar.gz` no Linux. Para Docker, há imagens prontas em `bageo/bageo-call` e `ghcr.io/h4lo-void/bageo-call`. No Docker, o celular pareia pelo endereço IP com um código do console web; veja [Phone setup → Wi-Fi](docs/en/PHONE.md#wi-fi).
2. **Instale e inicie.** Windows: clique duas vezes em `setup.exe` e depois no atalho **Bageo** da área de trabalho. macOS: abra o `.dmg`, arraste o Bageo para *Aplicativos* e clique nele. Linux: descompacte e execute `./start.sh`. O console abre em <http://localhost:8787/>; depois de definir uma senha do console, <http://bageo.local/> também funciona neste computador e na mesma rede Wi-Fi.
3. **Preencha o básico** em **Configurações**: sua chave de API do Gemini, seu nome e o idioma. Toque em ▶ para ouvir as vozes e escolha uma.
4. **Prepare o celular** uma única vez: instale `bageo-phone-module-<version>.zip` como módulo no Magisk ou KernelSU, reinicie, abra o app **Bageo**, siga as etapas de configuração dele e conecte o celular.
5. **Ligue para ele** de outro telefone: um toque e a IA atende.

Travou? Cada passo em detalhes: [Primeiros passos](docs/en/GETTING_STARTED.md) · [Configurar o celular](docs/en/PHONE.md).

## Documentação

A documentação detalhada ainda está só em inglês.

| | |
|---|---|
| **Começar** | [Primeiros passos](docs/en/GETTING_STARTED.md) · [Configurar o celular](docs/en/PHONE.md) · [Solução de problemas e perguntas frequentes](docs/en/TROUBLESHOOTING.md) |
| **Usar** | [Recursos](docs/en/FEATURES.md) · [Comandos de chat e controle das notificações](docs/en/CHAT_COMMANDS.md) · [Como escrever o prompt](docs/en/PROMPTS.md) · [Referência das configurações](docs/en/SETTINGS.md) · [Notificações](docs/en/NOTIFICATIONS.md) |
| **Ir mais fundo** | [Arquitetura](docs/en/ARCHITECTURE.md) · [API](docs/en/API.md) · [MCP](docs/en/MCP.md) · [Roteiro](docs/en/ROADMAP.md) · [Registro de alterações](CHANGELOG.md) · [`android/`](android/README.md) · [`tools/sms_forward/`](tools/sms_forward/README.md) |
| **Jurídico** | [Privacidade, segurança e a lei](docs/en/PRIVACY_AND_LEGAL.md) · [Aviso legal e termos para operadores](LEGAL.md) · [Licença dupla e comercial](COMMERCIAL_LICENSE.md) · [Auditoria de segurança e conformidade do projeto](AUDIT_REPORT.md) |

**Estado dos testes:** mais de 1.400 testes automáticos no CI, 234 ligações simuladas em 28 idiomas e ligações reais em Pixel 7, 7a e 9. [Detalhes](docs/en/FEATURES.md#test-status). Relatos de outros celulares e idiomas são os que mais ajudam: [CONTRIBUTING.md](CONTRIBUTING.md).

## Perguntas frequentes

<details>
<summary><strong>Uma IA pode atender ligações no meu número de telefone atual?</strong></summary>

Sim, é para isso que o Bageo existe. Coloque o seu SIM em um celular Android
com root que esteja sobrando, e a IA atende as ligações para o seu número e
faz ligações a partir dele. Para manter o SIM no seu celular principal,
redirecione as ligações não atendidas para o número do celular reserva: o seu
celular toca primeiro e as ligações perdidas vão para a IA (as ligações feitas
a seu pedido e os retornos vão então mostrar o número do celular reserva).

</details>

<details>
<summary><strong>Funciona sem root ou em um iPhone?</strong></summary>

Não. O Android só deixa apps privilegiados do sistema gravarem uma ligação e
tocarem som nela, por isso o celular precisa de Magisk ou KernelSU para
instalar o módulo do Bageo. O iOS não permite isso de jeito nenhum.

</details>

<details>
<summary><strong>Quais celulares são compatíveis?</strong></summary>

Os Google Pixel 7, 7a, 8 e 9 foram testados, com Android 10 ou mais novo. Outros
celulares com root só funcionam se a camada de áudio do hardware deixar um app
gravar a ligação e tocar som nela; se não, a ligação é atendida, mas um dos
lados não ouve nada. Veja
[Quais celulares](docs/en/PHONE.md#which-phones).

</details>

<details>
<summary><strong>Quanto custa manter funcionando?</strong></summary>

O Bageo é gratuito para uso pessoal e de código aberto sob `AGPL-3.0-only` (quem o operar como serviço hospedado de código fechado ou comercial precisa de uma [Licença Comercial](COMMERCIAL_LICENSE.md)). O Google cobra por uso o áudio do Gemini Live
de cada ligação que a IA atende, recebida ou feita; os resumos são pedidos
pequenos de texto ao modelo mais barato do Gemini. As prévias de voz e o aviso
de número bloqueado são pré-gravados e não custam nada. Some o seu plano de
celular e a energia do computador, e defina um orçamento na sua conta do Google
se quiser um limite.

</details>

<details>
<summary><strong>É legal deixar uma IA atender minhas ligações?</strong></summary>

Depende de onde você e a outra pessoa estão. Gravação de ligações, aviso de que
é uma IA e ligações automáticas têm cada uma as suas regras. O estilo
*Assistente de IA* diz que é uma IA já na primeira frase; *Como se fosse eu*
fala como você sem dizer que é uma IA. Leia
[Privacidade, segurança e a lei](docs/en/PRIVACY_AND_LEGAL.md) antes de
ligá-lo.

</details>

<details>
<summary><strong>Funciona offline ou sem o Google?</strong></summary>

Não. A voz ao vivo é o Gemini Live, que precisa de conexão com a internet e de
uma chave do Gemini. Os resumos podem rodar em um modelo local pelo Ollama
(veja [Resumo e agenda](docs/en/SETTINGS.md#summary--calendar)); uma pilha de
voz totalmente local não faz parte da configuração com suporte
([Roteiro](docs/en/ROADMAP.md)).

</details>

## Use com responsabilidade

- **As leis variam.** Gravar ligações, e deixar uma IA falar com pessoas sem
  avisar, é restrito em muitos lugares. O estilo de atendimento *Assistente de
  IA* diz de cara que é uma IA; as ligações feitas a seu pedido como **Seu
  assistente** apresentam a IA como seu assistente; *Como se fosse eu* e **Você
  em pessoa** falam como você sem avisar. Descubra o que vale onde você e a outra
  pessoa estão.
- **A IA não assume compromissos.** Nunca concorda com pagamentos, depósitos,
  contratos, senhas ou códigos, e os compromissos esperam a sua decisão. Ainda
  assim é uma IA: leia os resumos e os resultados.
- **Não conte com ela em emergências.** As ligações da IA nunca discam para
  números de emergência.
- **O root anula a garantia e pode dar errado.** Use um celular reserva.

## Licença e aviso legal

**Licença dupla** de **h4lo-void** (veja [LICENSING.md](LICENSING.md)):

- **Licença da comunidade de código aberto:** [GNU Affero General Public License v3.0 only (`AGPL-3.0-only`)](LICENSE), com os termos adicionais da Seção 7 em [LICENSING.md](LICENSING.md) — gratuita para uso pessoal, acadêmico e de código aberto. Quem modificar o Bageo e deixar outras pessoas interagirem com ele por uma rede ou por uma linha telefônica precisa publicar todo o código-fonte correspondente sob `AGPL-3.0-only`.
- **Obter uma Licença Comercial:** operadores, empresas, provedores de SaaS ou de call center e fabricantes de hardware que queiram implantar, incorporar ou hospedar o Bageo sem a divulgação de código-fonte exigida pela `AGPL-3.0-only` podem obter uma licença proprietária separada, por escrito — veja os [**Termos da Licença Comercial**](COMMERCIAL_LICENSE.md).

**Isenção de responsabilidade e leis locais:** o Bageo é um software hospedado por quem o usa, fornecido “NO ESTADO EM QUE SE ENCONTRA” (“AS IS”), sem garantia; o autor não opera nenhum serviço e não recebe dados de usuários. Quem o implanta é responsável por cumprir as leis locais sobre gravação de ligações, aviso de que se trata de uma IA e discagem automática, e também pelo root do aparelho e pelas cobranças da operadora e das APIs. Leia [**LEGAL.md**](LEGAL.md), [Privacidade, segurança e a lei](docs/en/PRIVACY_AND_LEGAL.md) e a [nota de revisão interna da v1.0 assistida por IA](AUDIT_REPORT.md) (`gemini-4-carbon`, não é uma auditoria de terceiros) antes de usar. Os avisos de terceiros (incluindo os dicionários `Apache-2.0` do OpenCC) estão em [NOTICE.md](NOTICE.md). Sem afiliação com a Google LLC; Pixel, Android e Gemini são marcas comerciais da Google LLC.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
