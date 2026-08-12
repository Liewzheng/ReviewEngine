# FAQ:API Token 与部署排查

> 部署后的**使用与排查**问答;部署时的首次设置见 [deploy/gitlab-ee.md](../deploy/gitlab-ee.md) 快速部署章节。

---

## API Token 使用

### Q1:API key(API Token)怎么设置?

二选一,详见 [快速部署章节 §3](../deploy/gitlab-ee.md):

- **Bootstrap 引导(推荐)**:`.env` 设 `REVIEW_BOOTSTRAP_KEY=$(openssl rand -hex 16)` → `docker compose up -d` → 打开 `http://<宿主IP>:18080`,引导页填自己创建的 **API Token** + .env 里的 **Bootstrap Key** → 保存后删掉 bootstrap key;
- **直接 env 设(兼容旧版)**:`.env` 设 `REVIEW_API_TOKEN=$(openssl rand -hex 32)` → `docker compose up -d` → 用该 token 登录。

登录端口:默认 `18080`(compose 映射 `18080:8080`);启用 HTTPS 后为 `443`。

### Q2:401 `unauthorized` 是怎么回事?

后端已配置了 token,但请求带的 token 与后端存储的**不一致**——填错、浏览器缓存了旧值、或把 bootstrap key 当成了日常 token。

排查:浏览器 F12 → **Application → Local Storage**,查 `review_engine_api_token` 的值,与后端实际 token 比对(不一致就删掉该键并重填)。

区别于 `auth_required`:后者表示后端**还没设任何 token**,应走首次引导界面(见 Q1 方式 A)。

### Q3:忘记 API token / token 失效了怎么办?

改 `.env` 重新注入新 token 并重建容器(env 优先于已存 token):

```bash
# 1. 改 .env:REVIEW_API_TOKEN=<新值>
# 2. 重建容器
docker compose up -d --force-recreate
# 3. 浏览器清掉旧缓存后重登
#    F12 → Application → Local Storage → 删除 review_engine_api_token → 刷新 → 输新 token
```

> 注意:UI 轮换(header 的 **API Token** 按钮)要求用**当前 token** 完成认证;token 已失效时 UI 无法自救(已知缺陷,下个版本修复),此时只能改 `.env` 重建。

### Q4:Bootstrap Key 和 API Token 有什么区别?

- **Bootstrap Key**(`REVIEW_BOOTSTRAP_KEY`):一次性引导凭证,只在**首次设置 API token** 时校验(非 loopback 部署需要),设完可删;
- **API Token**:日常认证凭证,所有请求经 `Authorization` 头携带;
- **不要把 bootstrap key 当日常 token 用**(会 401,见 Q2)。

---

## 容器部署排查

### Q5:启动报 `Bind mount failed: <dir> does not exist`?

Linux/NAS 上 bind mount 源目录不存在时**不会自动创建**,需先预建:

```bash
mkdir -p config reports bin frontend-dist auth tls
# tls 仅启用 HTTPS 时需要,其余必建
```

### Q6:容器日志报 `cp: Permission denied`(写卷失败)?

卷目录属主继承宿主用户,容器内非 root 用户(`review-engine`)写卷被拒,把属主改为容器 UID:

```bash
sudo chown -R <容器UID>:<容器UID> bin frontend-dist auth config reports tls
# UID 以 `docker run --rm --entrypoint id ghcr.io/liewzheng/review-engine:latest review-engine`
# 实测输出为准;v0.9.13+ 镜像固定 9001。Docker Desktop(macOS/Windows)无此问题。
```
