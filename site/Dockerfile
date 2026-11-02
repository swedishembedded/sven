FROM node:22-alpine AS build
WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --frozen-lockfile

COPY . .
RUN npm run build

FROM node:22-alpine
RUN npm install -g serve@14.2.5
WORKDIR /app
COPY --from=build /app/dist ./dist
COPY --from=build /app/dist/serve.json ./serve.json

EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s CMD wget -q --spider http://localhost:3000/ || exit 1
CMD ["serve", "dist", "-l", "3000"]
