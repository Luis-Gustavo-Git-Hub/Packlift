# Packlift

Packlift abre arquivos de pacote em uma interface GTK4 e libadwaita. Ele mostra a origem, a arquitetura e o método de instalação antes de executar qualquer instalador. Depois, cria atalhos no menu de aplicativos e registra os programas que gerencia.

| Arquivo | Instalação |
| --- | --- |
| `.deb` | APT em Debian e Ubuntu. Em outras distribuições, Toolbx com Ubuntu 24.04. |
| `.rpm` | DNF em Fedora e derivados de RHEL, ou Zypper em openSUSE. Em outras distribuições, Toolbx com Fedora. |
| `.pkg.tar.zst` | Pacman em Arch Linux, ou em uma imagem Toolbx do Arch. |
| `.AppImage` | Cópia executável no diretório de dados do Packlift. AppImages de tipo 2 abrem no modo de extração. |
| `.tar.gz`, `.tar.xz` | Extração local e identificação do executável. O Packlift pergunta quando encontra mais de uma opção. |
| `.exe`, `.msi`, `.setup` | Wine em um prefixo compartilhado, com atalhos convertidos para o desktop Linux. |

## Instalar o Packlift

O [GitHub Actions](https://github.com/Luis-Gustavo-Git-Hub/flatapk/actions) compila o arquivo `Packlift-x86_64.flatpak`. Baixe o artefato de um build concluído, extraia o ZIP e execute:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.gnome.Platform//49
flatpak install --user ./Packlift-x86_64.flatpak
flatpak run io.github.flatapk.Installer
```

Abra um pacote pelo gerenciador de arquivos com **Abrir com Packlift** ou pelo botão **Abrir pacote**. Se quiser usar o duplo clique sempre, escolha Packlift como aplicativo padrão para o formato nas propriedades do arquivo. Abra o Packlift sem selecionar um arquivo para gerenciar os programas instalados.

Se clicar no ícone de um AppImage e nenhuma janela aparecer, tente **Abrir** na lista de programas do Packlift. Se a execução encerrar com erro, a saída estará em **Ver detalhes**.

Pacotes nativos usam o gerenciador da distribuição e solicitam autorização via polkit. O Toolbx precisa estar instalado no host junto com o Podman. O Packlift reutiliza um ambiente por distribuição e usa Toolbx para repassar a sessão gráfica, o áudio e o diretório pessoal ao programa. Em sistemas Fedora baseados em OSTree, o Packlift usa Toolbx para RPMs. Os nomes das imagens podem ser configurados por `FLATAPK_UBUNTU_IMAGE`, `FLATAPK_FEDORA_IMAGE` e `FLATAPK_ARCH_IMAGE`.

### Windows

Em x86-64, o Packlift instala o Wine WoW64 do Flathub para sua conta quando o Wine do sistema estiver ausente. A primeira instalação requer internet e baixa também o runtime do Wine. O prefixo compartilhado fica em `~/.local/share/flatapk/wine/prefix` com o XDG padrão. O painel **Opções avançadas** aceita o caminho absoluto de um executável Wine ou do script `proton` instalado no host. Algumas distribuições de Proton exigem Steam e um runtime compatível no host.

Para um jogo já extraído, abra o executável `.exe` do jogo no Packlift e escolha **Adicionar jogo ao menu**. O atalho inicia o jogo na pasta do executável e mantém acesso aos arquivos da pasta pessoal. Para jogos em uma unidade externa, o Wine do Flathub recebe acesso à pasta do executável. A opção **Remover atalho** mantém os arquivos do jogo na pasta original.

O Packlift procura referências a componentes comuns do Windows e usa Winetricks quando detecta uma dependência conhecida. Instaladores compactados e jogos podem exigir ajustes manuais. Configure variáveis por programa, incluindo `WINEDLLOVERRIDES`, na lista de programas instalados.

Feche os programas do prefixo compartilhado antes de instalar, desinstalar ou reparar. Uma instalação malsucedida restaura a cópia anterior do prefixo quando possível. A última instalação do Windows pode ser desfeita pelo painel principal. O reparo recria o prefixo e executa novamente os instaladores preservados. O reparo pode exigir interação com os instaladores e apagar configurações ou documentos que estejam dentro do prefixo. A desinstalação normal usa o desinstalador registrado pelo programa quando disponível e preserva o prefixo compartilhado.

### Integridade e permissões

O manifesto permite que o Packlift execute ferramentas no host via `flatpak-spawn --host`, acesse arquivos na pasta pessoal e escreva atalhos no menu do sistema. Os aplicativos iniciados via Toolbx ou Wine têm acesso à pasta pessoal. Instale apenas pacotes de origens confiáveis.

O Packlift calcula o SHA-256 do arquivo preparado para instalação e verifica um arquivo vizinho com sufixo `.sha256` quando presente. Uma assinatura RPM é verificada com as chaves conhecidas pelo `rpmkeys` no host. Para assinaturas GPG separadas `.sig` e `.asc`, coloque um chaveiro OpenPGP com as chaves que você verificou em `~/.local/share/flatapk/trustedkeys.gpg` ou na pasta equivalente do XDG. A janela informa quando a assinatura do pacote não pôde ser verificada. Consulte os detalhes da operação ou o arquivo `flatapk.log` na pasta de dados do Packlift.

## Desenvolver

Requisitos para rodar a partir do código: Python 3.11+, PyGObject com GTK 4.10+ e libadwaita 1.4+, `bsdtar`/libarchive e as ferramentas dos formatos que deseja usar (`dpkg-deb`, `msiinfo`, `unsquashfs`, `zstd`). Para compilar as traduções, instale gettext.

```sh
make locale
python3 -m flatapk
python3 -m flatapk /caminho/para/programa.deb
make test
```

Para compilar o Flatpak x86-64 localmente, adicione o Flathub e execute:

```sh
flatpak-builder --user --install-deps-from=flathub --repo=flatpak-repo --force-clean \
  flatpak-build io.github.flatapk.Installer.json
flatpak build-bundle flatpak-repo Packlift-x86_64.flatpak io.github.flatapk.Installer
```

O runtime GNOME distribuído neste manifesto é x86-64. A execução nativa do código em um host x86 de 32 bits depende da disponibilidade de GTK, Wine e do gerenciador correspondente. As imagens atuais de Fedora e Arch usadas pelo Toolbx exigem um host de 64 bits. Em x86-64, pacotes i386 usam o multiarch de APT e pacotes i686 dependem das bibliotecas oferecidas pela distribuição RPM. O Packlift recusa arquivos especiais e links que apontam para fora do diretório do programa durante a extração.

A suíte automatizada exerce detecção por cabeçalho, planos de instalação, metadados de pacote real, arquivos locais, registro, atalhos, integridade, concorrência e recuperação do Wine. Para validar os fluxos externos, use sessões gráficas em Fedora e Ubuntu com Toolbx e em um host com Wine WoW64. O desempenho desses fluxos depende da imagem, dos repositórios e dos instaladores usados. Atualizações automáticas e prefixos isolados por programa estão previstas para a v2.

## Licença

GPL-3.0. Consulte o arquivo [LICENSE](LICENSE).
