# Запуск Picodata

В данном разделе приведена информация по запуску отдельного инстанса
Picodata на физическом оборудовании или в виртуальной среде.

[Инстанс](../overview/glossary.md#instance) — экземпляр приложения
Picodata, из которых состоит кластер. Picodata может создать кластер,
состоящий всего из одного экземпляра/инстанса.

## Минимальный вариант запуска {: #simple_run }

В самом простом случае запуск инстанса сводится к выполнению команды
 `picodata run` без каких-либо параметров:

```shell
picodata run
```

Будет запущен кластер, состоящий из одного инстанса. Все параметры будут
стандартны, и, скорее всего, вы захотите их переопределить.

Читайте далее:

- [Конфигурирование](../admin/configure.md)
- [Подключение и работа в консоли](../tutorial/connecting.md)

## Запуск нескольких инстансов {: #run_multiple_instances }

Для того чтобы запустить несколько инстансов на одном сервере,
потребуется задать дополнительные параметры для каждого из них:

- рабочую директорию ([`--instance-dir`])
- сетевой адрес ([`--iproto-listen`])
- адрес одного или нескольких соседних инстансов ([`--peer`])
- адрес сервера PostgreSQL ([`--pg-listen`])

Полный перечень возможных параметров запуска и их
описание содержатся в разделе [Аргументы командной строки], а также в
выводе команды `picodata run --help`.

[Аргументы командной строки]: ../reference/cli.md

Чтобы запустить два инстанса, которые объединятся в кластер,
выполните в двух соседних терминалах следующие команды:

```shell
picodata run --instance-dir ./data/i1 --iproto-listen 127.0.0.1:3301 --pg-listen 127.0.0.1:5432
```

```shell
picodata run --instance-dir ./data/i2 --iproto-listen 127.0.0.1:3302 --pg-listen 127.0.0.1:5433 --peer 127.0.0.1:3301
```

Обратите внимание на различия в запуске инстансов:

- каждый инстанс использует свою рабочую директорию и сетевой адрес
- в отсутствие параметра `--peer` запуск инстанса приводит к созданию
  нового кластера. Чтобы инстансы добавлялись в уже существующий кластер
  в этом параметре передается адрес первого инстанса

[`--instance-dir`]: ../reference/cli.md#run_instance_dir
[`--iproto-listen`]: ../reference/cli.md#run_iproto_listen
[`--peer`]: ../reference/cli.md#run_peer
[`--pg-listen`]: ../reference/cli.md#run_pg_listen

Читайте далее:

- [Создание кластера](../tutorial/deploy.md)

## Запуск с помощью Docker Compose {: #run_docker_compose }

Picodata поддерживает запуск при помощи инструментария Docker Compose и
предоставляет пример файла `docker-compose.yml` для запуска тестового
кластера. Этот способ удобен тем, что позволяет запускать Picodata без
необходимости [установки пакетов][available_packages] или [сборки из
исходного кода][installing_from_sources].

[available_packages]: install.md#available_packages
[installing_from_sources]: install.md#installing_from_sources

Для развертывания тестового кластера данным способом выполните следующие шаги.

Убедитесь, что у вас установлены Docker, Docker Compose, а также что
системная служба `docker` запущена, см [Docker Compose
overview](https://docs.docker.com/compose/)

Скачайте файл [docker-compose.yml], который описывает тестовый кластер
из 4-х инстансов:

[docker-compose.yml]: https://git.picodata.io/core/picodata/-/raw/master/docker/docker-compose.yml?inline=false

```bash
curl -O https://git.picodata.io/core/picodata/-/raw/master/docker/docker-compose.yml?inline=false
```

Создайте директорию для рабочих файлов проекта и
задайте путь к Docker-репозиторию Picodata:

```shell
mkdir pico
export REGISTRY=docker-public.binary.picodata.io
```

Запустите контейнеры:

```shell
docker-compose up -d
```

Подключиться к кластеру можно одним из следующих способов.

Для подключения к [консоли администратора](../tutorial/connecting.md#admin_console)
используйте команду:

```shell
picodata admin pico/data/picodata-1-1/admin.sock
```

Для подключения к [SQL-консоли](../tutorial/connecting.md#sql_console)
используйте команду:

```shell
picodata connect admin@127.0.0.1:13301
```

Пароль администратора `T0psecret` задан через
переменную окружения `PICODATA_ADMIN_PASSWORD`, см. [docker-compose.yml:17]

[docker-compose.yml:17]: https://git.picodata.io/core/picodata/-/blob/master/docker/docker-compose.yml#L17

Для подключения по протоколу [PostgreSQL](../tutorial/connecting.md#pgproto)
используйте команду:

```shell
psql postgres://admin@127.0.0.1:55432?sslmode=disable
```

## Безопасный запуск {: #secure_run }

Для обеспечения мер безопасности рекомендуется организовать хранение
пароля для внутреннего системного пользователя `pico_service` в
отдельном файле `.picodata-cookie`, который следует поместить в [рабочую
директорию инстанса]. Пароль пользователя `pico_service` используется в
рамках всего кластера, поэтому его нужно задать одинаковым для всех
инстансов кластера до их первого запуска (соответственно, следует также
заранее подготовить рабочие директории инстансов).

Дополнительно, в опции [`--audit`] явно укажите способ вывода [журнала
аудита](../admin/audit_log.md).

Также рекомендуется использовать опцию [`--shredding`], которая
обеспечивает безопасное удаление рабочих файлов.

Пример команд, реализующих безопасный запуск:

```shell
mkdir data
echo "shAreDs3cr3t" > data/.picodata-cookie
chmod 600 data/.picodata-cookie
picodata run --instance-dir=data --audit audit.log --shredding
```

[`--audit`]: ../reference/cli.md#run_audit
[`--shredding`]: ../reference/cli.md#run_shredding
[рабочую директорию инстанса]: ../reference/cli.md#run_instance_dir

## Безопасное завершение работы {: #secure_stop }

Для безопасного завершения работы инстанса нажмите сочетание
++ctrl+c++ в консоли, в которой он был запущен. После этого процесс
инстанса будет корректно завершен.
