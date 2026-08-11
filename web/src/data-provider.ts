import type {
  BaseKey,
  BaseRecord,
  CrudFilter,
  DataProvider,
  DeleteOneParams,
  DeleteOneResponse,
  GetListParams,
  GetListResponse,
  GetOneParams,
  GetOneResponse,
  Pagination,
  CreateParams,
  CreateResponse,
  UpdateParams,
  UpdateResponse,
} from "@refinedev/core";

import "@/api/client.ts";
import {
  createProviderAccount,
  deleteProviderAccount,
  getExecution,
  getProviderAccount,
  getTask,
  listExecutions,
  listProviderAccounts,
  listProviders,
  listTasks,
  updateProviderAccount,
} from "@/api/generated/sdk.gen.ts";
import type {
  CreateProviderAccount,
  UpdateProviderAccount,
} from "@/api/generated/types.gen.ts";
import { ensureSuccess, requireData } from "@/api/result.ts";

const API_URL = "/api/v1";

export const dataProvider: DataProvider = {
  getApiUrl: () => API_URL,
  getList: async <TData extends BaseRecord = BaseRecord>({ resource, pagination, filters }: GetListParams): Promise<GetListResponse<TData>> => {
    const { limit, offset } = paginationQuery(pagination);
    switch (resource) {
      case "providers": {
        const page = requireData(await listProviders());
        return { data: page.items as unknown as TData[], total: page.total };
      }
      case "provider-accounts": {
        const page = requireData(await listProviderAccounts());
        return { data: page.items as unknown as TData[], total: page.total };
      }
      case "tasks": {
        const page = requireData(
          await listTasks({
            query: {
              limit,
              offset,
              provider_account_id: stringFilter(filters, "provider_account_id"),
            },
          }),
        );
        return { data: page.items as unknown as TData[], total: page.total };
      }
      case "executions": {
        const page = requireData(
          await listExecutions({
            query: {
              limit,
              offset,
              task_id: stringFilter(filters, "task_id"),
            },
          }),
        );
        return { data: page.items as unknown as TData[], total: page.total };
      }
      default:
        throw new Error(`不支持的 Refine resource: ${resource}`);
    }
  },
  getOne: async <TData extends BaseRecord = BaseRecord>({ resource, id }: GetOneParams): Promise<GetOneResponse<TData>> => {
    const value = String(id);
    switch (resource) {
      case "providers": {
        const providers = requireData(await listProviders()).items;
        const provider = providers.find((item) => item.id === value);
        if (!provider) throw new Error("Provider 不存在");
        return { data: provider as unknown as TData };
      }
      case "provider-accounts":
        return {
          data: requireData(
            await getProviderAccount({ path: { account_id: value } }),
          ) as unknown as TData,
        };
      case "tasks":
        return {
          data: requireData(await getTask({ path: { task_id: value } })) as unknown as TData,
        };
      case "executions": {
        const detail = requireData(
          await getExecution({ path: { execution_id: value } }),
        );
        return {
          data: { ...detail, id: detail.execution.id } as unknown as TData,
        };
      }
      default:
        throw new Error(`不支持的 Refine resource: ${resource}`);
    }
  },
  create: async <TData extends BaseRecord = BaseRecord, TVariables = Record<string, unknown>>({ resource, variables }: CreateParams<TVariables>): Promise<CreateResponse<TData>> => {
    if (resource !== "provider-accounts") {
      throw new Error(`resource ${resource} 不支持 create`);
    }
    return {
      data: requireData(
        await createProviderAccount({ body: variables as CreateProviderAccount }),
      ) as unknown as TData,
    };
  },
  update: async <TData extends BaseRecord = BaseRecord, TVariables = Record<string, unknown>>({ resource, id, variables }: UpdateParams<TVariables>): Promise<UpdateResponse<TData>> => {
    if (resource !== "provider-accounts") {
      throw new Error(`resource ${resource} 不支持 update`);
    }
    return {
      data: requireData(
        await updateProviderAccount({
          path: { account_id: String(id) },
          body: variables as UpdateProviderAccount,
        }),
      ) as unknown as TData,
    };
  },
  deleteOne: async <TData extends BaseRecord = BaseRecord, TVariables = Record<string, unknown>>({ resource, id }: DeleteOneParams<TVariables>): Promise<DeleteOneResponse<TData>> => {
    if (resource !== "provider-accounts") {
      throw new Error(`resource ${resource} 不支持 delete`);
    }
    ensureSuccess(
      await deleteProviderAccount({ path: { account_id: String(id) } }),
    );
    return { data: { id } as TData };
  },
};

function paginationQuery(pagination?: Pagination): { limit: number; offset: number } {
  const limit = pagination?.pageSize ?? 50;
  const currentPage = pagination?.currentPage ?? 1;
  return { limit, offset: Math.max(0, currentPage - 1) * limit };
}

function stringFilter(filters: CrudFilter[] | undefined, field: string): string | undefined {
  const filter = filters?.find(
    (candidate) => "field" in candidate && candidate.field === field,
  );
  if (!filter || !("value" in filter) || filter.value === undefined || filter.value === null) {
    return undefined;
  }
  return String(filter.value);
}

export function resourceId(id: BaseKey): string {
  return String(id);
}
