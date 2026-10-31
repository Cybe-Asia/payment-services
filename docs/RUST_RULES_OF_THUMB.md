# Rust Rules of Thumb

> Revision 2026-10-31 (Digital School audit): added measurable SOLID and DRY checks and the shared service crate (§2), one error mapping (§11), CPU work off the runtime (§18), secrets without fallbacks and clients built once (§21), query rules for Neo4j and SQL (§23), authentication hardening (§24), CI gates (§26), and an enforcement map (§31). Most audit findings already broke existing rules; §31 exists so they cannot be broken silently again.

## 1. Technology and Architecture

- Rust
- Clean Architecture
- SOLID Principles
- DRY Principle
- Package and module-oriented design
- Dependency Injection
- Repository Pattern
- Datasource Layer
- Service Layer
- Use Case Layer
- `Result<T, E>` for recoverable errors
- `Option<T>` for optional values
- `async/await` with Tokio when asynchronous execution is required
- `serde` for serialization and deserialization
- Axum, Actix Web, or another suitable web framework when building APIs
- SQLx, Diesel, SeaORM, or another suitable database library when required
- Unit and integration testing

## 2. Core Principles

- Prefer simple and readable code.
- Follow SOLID principles where they provide real value.
- Follow the DRY principle, but do not over-abstract.
- Prefer composition over inheritance.
- Keep modules focused on one responsibility.
- Make ownership and data flow explicit.
- Prefer compile-time guarantees over runtime checks where practical.
- Avoid unnecessary cloning.
- Avoid unnecessary allocations.
- Keep business logic independent from frameworks and infrastructure.
- Do not expose implementation details between layers.
- Use dependency inversion at architectural boundaries.
- Avoid premature optimization.
- Avoid premature abstraction.
- Every abstraction must have a clear purpose.
- Every public API should be easy to understand and test.

### SOLID in practice

- **Single responsibility:** one module per workflow. A handler file above about 400 lines, or one that both serves HTTP and runs a multi-step workflow, must be split. Routing, cookie crypto, external transport and token endpoints do not share a module.
- **Open/closed:** variation by status, role or kind uses enums with exhaustive `match`, not string comparison.
- **Liskov substitution:** every implementation of a trait (database, in-memory, fake) honours the same contract, including errors. Run one shared contract test against each implementation.
- **Interface segregation:** a repository trait serves one workflow. A trait above about 10 methods is split (for example draft, approval, payment).
- **Dependency inversion:** use cases and services depend on traits or concrete types passed in; they never take `&neo4rs::Graph`, `PgPool` or a `reqwest::Client` directly. Construction happens in `main.rs` or `app.rs`.
- Domain rules live once in the domain. Adapters (database, in-memory) call the rule; they do not re-implement it.

### DRY in practice

- **Rule of three:** a second copy is acceptable; the third copy of the same logic becomes a function or an extension trait in the service's `shared` module.
- **Across services:** code needed by two or more services lives in one shared crate (for example `ds-service-kit`) and is never copied. It holds: JWT and staff session verification, the `ApiResponse` envelope, the `AppError` to HTTP mapping, the HTTP client builder with timeouts, private CA loading, config helpers, and tracing setup.
- **Knowledge before code:** error codes, status values, header names, timeouts and environment variable names have exactly one definition.
- **Do not merge coincidences:** two blocks that look alike but change for different reasons stay separate.

## 3. Dependency Flow

Dependencies should flow inward:

```text
Handler / Controller
    ↓
Use Case
    ↓
Service
    ↓
Repository
    ↓
Datasource
    ↓
API / Database / External Service
```

Rules:

- Handlers must not contain business logic.
- Use cases represent business actions.
- Services contain reusable business logic or technical capabilities.
- Repositories define data access contracts.
- Datasources communicate with databases or external APIs.
- Domain code must not depend on HTTP, database drivers, ORM implementations, or external SDKs.
- DTOs and database models must not leak into the domain layer.
- Convert external DTOs and database models into domain models at the data boundary.
- Each use case should represent one main business action.
- Avoid architectural layers that only forward calls without adding value.
- Layers are mandatory only when they do work. A handler may call a repository trait directly for a plain read; a use case exists when there is a rule, a validation or more than one write.

## 4. Layer Responsibilities

### Presentation or Transport Layer

Responsible for:

- HTTP handlers
- gRPC handlers
- Request parsing
- Response mapping
- Authentication and authorization boundaries
- Input validation
- Middleware

Must not contain:

- Complex business rules
- SQL queries
- Database-specific logic
- External API implementation details

### Application Layer

Responsible for:

- Use cases
- Application workflows
- Business orchestration
- Calling repositories and services
- Coordinating transactions

Must not contain:

- HTTP-specific implementation details
- SQL-specific implementation details
- Framework-specific response formatting

### Domain Layer

Responsible for:

- Domain models
- Business rules
- Value objects
- Domain errors
- Repository contracts
- Typed statuses and flags
- Domain constants

Must not depend on:

- Web frameworks
- Database drivers
- ORM implementations
- HTTP clients
- External SDKs
- Serialization libraries unless required by the domain itself

### Infrastructure and Data Layer

Responsible for:

- Repository implementations
- Datasources
- Database access
- HTTP clients
- External service integrations
- DTOs
- Database entities
- Serialization and deserialization
- Mapping between external data and domain models

## 5. Project Structure

Prefer a feature-oriented structure:

```text
.
├── Cargo.toml
├── Cargo.lock
├── migrations/
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── config/
│   │   ├── mod.rs
│   │   └── settings.rs
│   ├── shared/
│   │   ├── constants.rs
│   │   ├── errors.rs
│   │   ├── pagination.rs
│   │   └── types.rs
│   ├── platform/
│   │   ├── database/
│   │   ├── http_client/
│   │   ├── logger/
│   │   └── telemetry/
│   ├── features/
│   │   └── users/
│   │       ├── domain/
│   │       │   ├── entities.rs
│   │       │   ├── value_objects.rs
│   │       │   ├── errors.rs
│   │       │   └── repository.rs
│   │       ├── application/
│   │       │   ├── use_cases/
│   │       │   └── services/
│   │       ├── infrastructure/
│   │       │   ├── dto.rs
│   │       │   ├── mappers.rs
│   │       │   ├── repositories/
│   │       │   └── datasources/
│   │       └── presentation/
│   │           ├── handlers.rs
│   │           ├── requests.rs
│   │           └── responses.rs
│   └── routes.rs
└── tests/
    ├── integration/
    └── common/
```

Use feature-specific modules for code that belongs to one feature.

Use `shared/` only for code that is genuinely reused across multiple features.

Avoid one large `utils` module containing unrelated functionality.

## 6. Module and File Naming

Use lowercase snake case for files and modules:

```text
user_handler.rs
user_service.rs
user_use_case.rs
user_repository.rs
user_repository_impl.rs
user_datasource.rs
user_datasource_impl.rs
user_dto.rs
user_mapper.rs
user_constants.rs
```

Recommended naming rules:

- Modules: `snake_case`
- Types and traits: `PascalCase`
- Functions and variables: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- Traits should describe behavior, for example `UserRepository`.
- Concrete implementations should use descriptive names, not `Impl` when possible.

Prefer:

```rust
pub struct SqlUserRepository {
    datasource: UserDatasource,
}
```

Avoid:

```rust
pub struct UserRepositoryImpl {
    // Implementation details
}
```

## 7. File Size Guidelines

File size is a guideline, not an absolute rule.

- Handler: ideally 50–150 lines
- Middleware: ideally 30–100 lines
- Service: ideally 50–150 lines
- Repository: ideally 50–150 lines
- Datasource: ideally 50–150 lines
- Use Case: ideally 20–80 lines
- Domain model: ideally 30–150 lines
- Mapper: ideally 20–100 lines
- Module: ideally 100–300 lines
- More than 250–300 lines should be reviewed and potentially split

Rules:

- Split modules by responsibility, not only by line count.
- A small file can still have bad architecture.
- A large file is acceptable when its responsibility is cohesive.
- Files with many branches, mappings, or nested logic should be reviewed.
- A module with unrelated domain concepts must be split.
- Do not split files only to meet a line-count target.

## 8. Ownership and Borrowing

- Borrow by default when a function does not need ownership.
- Take ownership when a value must be stored, moved, or transformed.
- Use `&str` for read-only string input.
- Use `String` when ownership is required.
- Use slices such as `&[T]` for borrowed collections.
- Avoid unnecessary `.clone()` calls.
- Every `clone()` should have a clear reason.
- Prefer borrowing over cloning when lifetimes remain simple.
- Do not use `Arc<Mutex<T>>` by default.
- Prefer ownership, message passing, or scoped mutation before shared mutable state.
- Use `Cow` only when it provides a real benefit.
- Keep lifetime annotations simple whenever possible.
- Do not hide ownership behavior behind unnecessary wrappers.

Example:

```rust
fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}
```

## 9. Domain Models

- Use domain types instead of primitive obsession.
- Use newtypes for important IDs and domain values.
- Use enums for finite business states.
- Use value objects for values with validation rules.
- Do not represent important business states with unrelated booleans.
- Keep invariants inside constructors or domain methods.
- Do not allow invalid domain objects to be created easily.
- Keep domain models independent from database and API schemas.
- Use explicit conversions between DTOs, entities, and domain models.
- Prefer `TryFrom` when conversion can fail.
- Prefer `From` when conversion cannot fail.

Example:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserId(String);

impl UserId {
    pub fn new(value: String) -> Result<Self, UserIdError> {
        if value.trim().is_empty() {
            return Err(UserIdError::Empty);
        }

        Ok(Self(value))
    }
}
```

## 10. Constants and Typed Values

Statuses, flags, error codes, event names, routes, keys, and repeated strings must use constants or typed values.

Use constants for:

- Status values
- Flags
- Roles
- Permissions
- Event names
- Error codes
- Route names
- API paths
- Storage keys
- Header names
- Feature keys
- Pagination defaults
- Sentinel values
- Empty strings when they have semantic meaning

Prefer enums for finite business values:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserStatus {
    Active,
    Inactive,
}
```

If a string representation is required, use typed constants:

```rust
pub const USER_STATUS_ACTIVE: &str = "active";
pub const USER_STATUS_INACTIVE: &str = "inactive";
```

Prefer a typed string wrapper when the value crosses an API or storage boundary:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status(String);

impl Status {
    pub const ACTIVE: &'static str = "active";
    pub const INACTIVE: &'static str = "inactive";
}
```

Other examples:

```rust
pub const EVENT_USER_CREATED: &str = "user.created";
pub const EVENT_USER_UPDATED: &str = "user.updated";

pub const ERROR_CODE_UNAUTHORIZED: &str = "UNAUTHORIZED";
pub const ERROR_CODE_NOT_FOUND: &str = "NOT_FOUND";
pub const ERROR_CODE_VALIDATION_FAILED: &str = "VALIDATION_FAILED";

pub const API_PATH_USERS: &str = "/api/users";
pub const STORAGE_KEY_ACCESS_TOKEN: &str = "access_token";
pub const EMPTY_STRING: &str = "";
```

Rules:

- Do not scatter raw status strings throughout the codebase.
- Do not compare important business values against raw string literals.
- Group constants by feature or domain.
- Avoid one large global constants module.
- Use typed constants instead of untyped values when possible.
- Use enums when values are finite and controlled by the application.
- Use constants when values must be serialized as strings.
- User-facing strings should be centralized or localized at the presentation boundary.
- Strings used only once do not need to become constants unless they have business meaning.
- Do not create a global constant only to avoid writing a simple literal once.

Avoid:

```rust
if user.status == "active" {
    // Repeated raw string
}
```

Prefer:

```rust
if user.status == UserStatus::Active {
    // Business logic
}
```

## 11. Error Handling

- Return `Result<T, E>` for recoverable errors.
- Use `Option<T>` for values that may not exist.
- Never ignore errors without a clear reason.
- Add context when propagating errors.
- Use `%` formatting with error context and preserve the source error.
- Use typed domain errors where callers need to react differently.
- Do not expose internal errors directly through an API.
- Map domain and infrastructure errors into transport errors at the handler boundary.
- Avoid `panic!` for expected runtime failures.
- Use `expect()` only for documented invariants that cannot fail under valid conditions.
- Avoid `unwrap()` in production application paths.
- Preserve error sources for logging and debugging.
- Each service has one `AppError` that implements `IntoResponse`. It logs the source error once and returns a stable code with a fixed public message. Handlers return `Result<_, AppError>`.
- Never send `e.to_string()` of a driver, database or HTTP client error to a client.
- New code does not return `Result<_, String>`. Never choose an HTTP status by matching error message text.

Example:

```rust
#[derive(Debug, thiserror::Error)]
pub enum CreateUserError {
    #[error("user already exists")]
    AlreadyExists,

    #[error("invalid email")]
    InvalidEmail,

    #[error("repository error")]
    Repository(#[from] UserRepositoryError),
}
```

Use `thiserror` for typed domain and library errors.

Use `anyhow` at application boundaries when preserving detailed context is more important than matching specific errors.

## 12. Traits and Abstractions

- Define traits at the layer that needs the behavior.
- Keep traits small and focused.
- Use traits for repositories, external services, clocks, and replaceable boundaries.
- Do not create a trait for every struct automatically.
- Prefer concrete types until substitution, mocking, or multiple implementations is required.
- Use generics when static dispatch is preferred.
- Use `dyn Trait` when runtime substitution is required.
- Avoid large traits with unrelated methods.
- Prefer multiple focused traits over one large trait.
- Do not create a trait only to satisfy an architectural diagram.

Example:

```rust
#[async_trait::async_trait]
pub trait UserRepository: Send + Sync {
    async fn get_by_id(&self, id: &UserId) -> Result<Option<User>, UserRepositoryError>;
}
```

## 13. Use Case Rules

- Each use case should represent one business action.
- Use cases should be framework-independent.
- Use cases should be small and easy to test.
- Use cases may orchestrate repositories and services.
- Use cases should validate business rules.
- Use cases should return domain models or application-specific results.
- Use cases must not return raw DTOs or database entities.
- Use cases must not depend on HTTP response types.
- Prefer an explicit method such as `execute`.
- `invoke` may be used if it is the project-wide convention, but `execute` is idiomatic in Go and Rust codebases.

Example:

```rust
pub struct CreateUserInput {
    pub name: String,
    pub email: String,
}

pub struct CreateUserUseCase<R> {
    repository: R,
}

impl<R> CreateUserUseCase<R>
where
    R: UserRepository,
{
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        input: CreateUserInput,
    ) -> Result<User, CreateUserError> {
        let user = User::create(input.name, input.email)?;

        self.repository
            .create(user)
            .await
            .map_err(CreateUserError::Repository)
    }
}
```

## 14. Service Rules

- Services contain reusable business or technical capabilities.
- Services must have one focused responsibility.
- Avoid “god services”.
- External integrations should be wrapped behind traits.
- Third-party SDKs must not be exposed directly to handlers or use cases.
- Services should return application-specific results or typed errors.
- Business services belong in the domain or application layer.
- External integration services belong in the infrastructure layer.
- Do not create a service for a simple one-line operation.
- Do not put repository implementation details inside services.

Examples:

```text
EmailService
PaymentService
AnalyticsService
NotificationService
FileStorageService
AuthenticationService
```

## 15. Repository Rules

- Repository traits define data access contracts.
- Repository implementations belong to the infrastructure layer.
- Repository traits and implementations should be placed in separate files or modules.
- Repository traits should be defined by the domain or application consumer.
- Repositories should return domain models.
- Repositories should map database records and external DTOs into domain models.
- Repositories must not expose SQL rows, ORM models, or HTTP response types.
- Repositories coordinate datasources.
- Repositories define clear transaction boundaries.
- Repositories should define clear caching behavior when caching is required.
- Repositories must not contain presentation logic.

Recommended files:

```text
user_repository.rs
user_repository_impl.rs
```

Example:

```rust
#[async_trait::async_trait]
pub trait UserRepository: Send + Sync {
    async fn get_by_id(&self, id: &UserId) -> Result<Option<User>, UserRepositoryError>;
    async fn create(&self, user: User) -> Result<User, UserRepositoryError>;
}
```

```rust
pub struct SqlUserRepository<D> {
    datasource: D,
}

impl<D> SqlUserRepository<D> {
    pub fn new(datasource: D) -> Self {
        Self { datasource }
    }
}
```

## 16. Datasource Rules

- Datasources communicate with databases or external APIs.
- Datasource traits and implementations should be placed in separate files or modules.
- Datasources must not contain business rules.
- Datasources must not contain presentation logic.
- Datasources should return DTOs or infrastructure models.
- Keep SQL queries, HTTP requests, serialization, and retry logic inside the datasource layer.
- Standardize timeout, authentication, headers, and error parsing.
- Convert database and HTTP errors into application-specific infrastructure errors.
- Do not expose database client details outside the datasource layer.
- Use transactions when multiple writes must succeed or fail together.
- Keep database-specific code inside the datasource implementation.

Recommended files:

```text
user_remote_datasource.rs
user_remote_datasource_impl.rs
user_local_datasource.rs
user_local_datasource_impl.rs
```

## 17. Handler and API Rules

- Handlers should parse input, authenticate, authorize, call a use case, and map the response.
- Handlers must not contain business workflows.
- Validate request payloads at the API boundary.
- Use typed request and response models.
- Standardize API response and error formats.
- Do not return database models directly.
- Keep HTTP-specific types inside the presentation or transport layer.
- Use middleware for authentication, logging, tracing, rate limiting, and recovery.
- Define request timeouts.
- Do not expose sensitive information through API responses.
- Keep handlers thin and easy to test.

## 18. Context and Async Rules

- Pass `&Context` or `CancellationToken` to operations that need cancellation or deadlines.
- Keep cancellation behavior explicit.
- Do not create unbounded background tasks.
- Every spawned task must have a clear owner and shutdown path.
- Handle errors from spawned tasks.
- Avoid blocking an async runtime with synchronous I/O or CPU-heavy work.
- Use bounded concurrency for external calls.
- Do not hold a lock across an `.await` unless it is explicitly safe.
- Use `tokio::time::timeout` for network operations when appropriate.
- Avoid unnecessary async functions.
- Use synchronous code when no asynchronous work is required.
- Prefer message passing or ownership over shared mutable state.
- Use `Arc` only when shared ownership is required.
- Use `Mutex` or `RwLock` only when shared mutation is required.
- CPU-heavy work runs in `tokio::task::spawn_blocking`: password hashing and verification (bcrypt, argon2), PDF and image rendering, large serialisation.
- Random high-entropy secrets (refresh tokens, API keys) are hashed with SHA-256, not a password hash.
- No `std::fs`, `reqwest::blocking` or other blocking I/O inside a request handler; load files at startup.

## 19. Dependency Injection

- Prefer constructor injection.
- Create dependencies in `main.rs` or a dedicated composition root.
- Do not create repositories inside handlers.
- Do not use a service locator as the primary dependency mechanism.
- Avoid global mutable state.
- Inject traits at architectural boundaries.
- Use concrete types for internal dependencies when substitution is not required.
- Provide fake implementations for tests.
- Keep dependency wiring separate from business logic.
- Make ownership and lifetimes explicit in dependency construction.

Example:

```rust
#[tokio::main]
async fn main() -> Result<(), AppError> {
    let database = create_database().await?;
    let datasource = SqlUserDatasource::new(database.clone());
    let repository = SqlUserRepository::new(datasource);
    let use_case = CreateUserUseCase::new(repository);
    let handler = UserHandler::new(use_case);

    let router = create_router(handler);
    serve(router).await?;

    Ok(())
}
```

## 20. Wrapper Rules

Create wrappers when they provide type safety, consistency, or a clear boundary.

Recommended wrappers:

- Typed IDs such as `UserId`.
- Typed domain values such as `Email` or `Money`.
- `ApiResponse<T>` for standardized API responses.
- `ApiError` for transport-level errors.
- Pagination wrappers.
- Repository traits.
- External SDK wrappers.
- HTTP client wrappers.
- Database client wrappers.
- Transaction wrappers.
- Middleware layers.
- Error mapping utilities.
- Configuration wrappers for validated values.

Example:

```rust
#[derive(Debug, serde::Serialize)]
pub struct ApiResponse<T> {
    pub data: T,
    pub message: Option<String>,
    pub meta: Option<ApiMeta>,
}
```

Avoid:

- `BaseService` without meaningful shared behavior.
- `BaseRepository` without meaningful shared behavior.
- Wrappers that only forward method calls.
- Excessive nested wrappers.
- Wrapping every struct without adding type safety or behavior.
- Hiding ownership, errors, or lifecycle behavior behind abstractions.

## 21. Configuration Rules

- Load configuration at application startup.
- Validate required environment variables at startup.
- Use a typed configuration struct.
- Do not read environment variables throughout the codebase.
- Do not hardcode secrets.
- Keep configuration separate from business logic.
- Use separate configuration for development, staging, and production.
- Fail fast when required configuration is missing.
- Do not expose secrets in logs or API responses.
- Secrets have no default. `env::var("JWT_SECRET").unwrap_or_else(|_| "…")` is a defect; a missing or short secret (under 32 bytes) stops the service at startup.
- Environment variables are read once, in the config module. Handlers and repositories receive values through state.
- HTTP clients are built once at startup with a connect timeout and a request timeout, and shared through state. Never build a client per request.

Example:

```rust
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub server_port: u16,
}
```

## 22. Serialization and DTO Rules

- Keep API request and response types separate from domain models.
- Keep database entities separate from domain models.
- Use DTOs at external boundaries.
- Map DTOs into domain models explicitly.
- Use `serde` only at serialization boundaries when possible.
- Do not serialize internal implementation details accidentally.
- Validate deserialized data before constructing domain models.
- Do not allow arbitrary `HashMap<String, Value>` data for core business models.

## 23. Database Rules

- Keep database access inside the datasource layer.
- Use migrations for schema changes.
- Make transaction ownership explicit.
- Do not pass database connections through every layer unnecessarily.
- Keep database entities separate from domain entities.
- Use indexes based on actual query patterns.
- Avoid N+1 queries.
- Define clear connection pool settings.
- Handle timeouts and connection failures.
- Use integration tests for important database workflows.
- Do not build SQL strings from untrusted input.
- Match on indexed or unique properties. Normalise at write time (for example `normalizedEmail`) instead of `toLower(prop) = …` in a `MATCH`, which cannot use an index.
- Every property used to look up a node or row in a request path has an index or constraint in a migration.
- No `.await` on a query inside a loop over query results. Fetch related data in the same query (`OPTIONAL MATCH … collect()`, `UNWIND`, or a SQL join).
- A workflow that writes more than once runs in one transaction.
- List queries always have `LIMIT` and pagination.

## 24. Security Rules

- Never hardcode secrets, tokens, or credentials.
- Validate all external input.
- Use parameterized database queries.
- Do not log sensitive data.
- Hash passwords using an appropriate password hashing algorithm.
- Use secure session and token handling.
- Apply authorization at the use-case or policy boundary.
- Do not rely only on frontend authorization.
- Return safe error messages to external clients.
- Keep dependency versions updated and audited.
- Use least-privilege credentials.
- Apply rate limiting where appropriate.
- Login, OTP and password reset have per-IP and per-account rate limits.
- Authentication failures return 401 with the same response and similar timing for unknown and known accounts (verify against a dummy hash when the account does not exist).
- Compare tokens, webhook secrets and signatures in constant time (`subtle`).
- Every route that sends email, messages or money requires authentication, including internal-only routes (internal token or network policy).
- Logs carry IDs, not emails, phone numbers or names.

## 25. Testing Rules

- Unit test domain rules and use cases independently from infrastructure.
- Use fake repositories or mock traits for use-case tests.
- Test repository implementations with a real or disposable database when possible.
- Test handlers for validation, response mapping, and error mapping.
- Test success and failure paths.
- Test authorization rules.
- Add regression tests for bug fixes.
- Prefer deterministic tests.
- Avoid tests that depend on execution order.
- Do not use real external APIs in unit tests.
- Use integration tests for cross-layer workflows.
- Use end-to-end tests for critical user flows.
- Use property-based testing when it provides meaningful coverage.

Example:

```rust
#[tokio::test]
async fn creates_a_user() {
    let repository = InMemoryUserRepository::default();
    let use_case = CreateUserUseCase::new(repository);

    let result = use_case
        .execute(CreateUserInput {
            name: "John".to_owned(),
            email: "john@example.com".to_owned(),
        })
        .await;

    assert!(result.is_ok());
}
```

## 26. Tooling and Quality

- Run `cargo fmt` before committing.
- Run `cargo check` during development.
- Run `cargo test`.
- Run `cargo clippy --all-targets --all-features -- -D warnings`.
- Run `cargo test --all-features` when appropriate.
- Run the race detector or concurrency checks where applicable.
- Use structured logging and tracing.
- Do not use `println!` for application logging.
- Keep compiler warnings at zero.
- Keep linter warnings at zero.
- Use dependency auditing tools such as `cargo audit` where appropriate.
- Measure performance before optimizing.
- Avoid unsafe code unless it is necessary, isolated, and documented.
- Document safety assumptions for every `unsafe` block.
- CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` before the image build, and `cargo deny check` or `cargo audit`. A workflow that only builds an image is incomplete.
- Lints live in `Cargo.toml` (`[lints]`), including `clippy::unwrap_used` and `clippy::expect_used` as warnings outside tests. Module-wide `#![allow(dead_code)]` is not used.

## 27. Logging and Observability

- Use structured logs.
- Use `tracing` for asynchronous applications.
- Include request IDs or correlation IDs where appropriate.
- Do not log passwords, tokens, credentials, or personal data.
- Add useful context to errors.
- Avoid logging the same error multiple times at different layers.
- Record metrics for important external calls and workflows.
- Add tracing around database and API calls where useful.
- Keep observability code outside domain logic.

## 28. Performance Rules

- Prefer clear code before micro-optimizations.
- Measure before optimizing.
- Avoid unnecessary allocations.
- Avoid unnecessary cloning.
- Use references when ownership is not required.
- Use bounded concurrency for external work.
- Avoid blocking async executors.
- Use connection pooling appropriately.
- Avoid N+1 database queries.
- Stream large data where appropriate.
- Use caching only with a clear invalidation strategy.
- Do not sacrifice correctness for premature performance improvements.

## 29. Code Quality Rules

- Use explicit and meaningful names.
- Keep functions short and focused.
- Prefer early returns where they improve readability.
- Avoid deeply nested conditionals.
- Avoid excessive generics.
- Avoid macros unless they improve clarity or remove meaningful repetition.
- Avoid unnecessary trait bounds.
- Avoid hidden global state.
- Keep unsafe code isolated.
- Keep side effects at infrastructure boundaries.
- Keep public APIs minimal.
- Document public APIs when they are part of a reusable crate.
- Avoid circular module dependencies.
- Avoid unrelated functionality in shared modules.

## 30. Pull Request Checklist

Before creating a pull request, verify that:

- Business logic is not inside handlers.
- Domain code does not depend on infrastructure.
- Repository traits and implementations are separated.
- Datasource traits and implementations are separated.
- Database models and API DTOs do not leak into the domain.
- Statuses, flags, routes, keys, and error codes use typed constants or enums.
- Raw business strings are not scattered throughout the codebase.
- Recoverable failures use `Result` or `Option`.
- Errors preserve useful context and sources.
- `unwrap()` and `expect()` are not used for expected runtime failures.
- Context and cancellation are propagated correctly.
- No goroutine or async task leaks exist.
- Async code does not block the runtime.
- No unnecessary global state or singleton behavior exists.
- Dependencies are injected through constructors.
- Configuration is validated at startup.
- Secrets are not hardcoded or logged.
- API responses and errors follow a consistent format.
- DTOs are mapped into domain models.
- Unit tests cover important business rules.
- Integration tests cover important data workflows.
- `cargo fmt` passes.
- `cargo check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo test` passes.
- No module has too many unrelated responsibilities.
- Wrappers and abstractions provide real value.
- The code follows SOLID and DRY principles.
- Nothing is copied from another service; shared code goes to the shared crate.
- No secret has a fallback value; no environment read outside config.
- No blocking or CPU-heavy work on the async runtime.
- No query inside a loop over rows; multi-write workflows use a transaction.

## 31. Enforcement

A rule without an automatic check drifts. These checks run in CI before the image build; a rule moves to "enforced" only when its check exists.

| Rule | Check |
|---|---|
| Formatting, warnings (§26) | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` |
| No `unwrap`/`expect` in request paths (§11) | `[lints.clippy] unwrap_used = "warn"`, `expect_used = "warn"`, allowed in tests |
| Secrets without fallback (§21) | Grep guard: `env::var(` only in the config module; no `unwrap_or` on secret names |
| Layer direction (§3) | Grep guard: no `axum`, `neo4rs`, `sqlx`, `reqwest` imports under `domain/` (as in expense-services `scripts/check-architecture.sh`) |
| One error mapping (§11) | Grep guard: no `e.to_string()` passed to a response constructor |
| Dependencies audited (§24) | `cargo deny check` |
| Tests (§25) | `cargo test` |
